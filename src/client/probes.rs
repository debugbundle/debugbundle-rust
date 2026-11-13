use super::{Client, Inner, State};
use crate::{probes::Directive, Context, ProbeOptions};
use serde_json::{json, Value};
use std::sync::{atomic::Ordering, Arc};

impl Client {
    /// Bind a locally verified token to one active request owner. Raw tokens are
    /// borrowed only during validation and never enter a retained context.
    pub fn bind_probe_trigger(
        &self,
        operation: &crate::OperationContext,
        header: Option<&str>,
        query: Option<&str>,
    ) -> bool {
        if crate::runtime::in_callback() {
            return false;
        }
        let Some(inner) = &self.inner else {
            return false;
        };
        if !operation.bind_trigger(&inner.operations, None) {
            return false;
        }
        let Ok(state) = inner.state.try_lock() else {
            return false;
        };
        if !state.remote.remote_enabled || inner.stopped.load(Ordering::Acquire) {
            return false;
        }
        let Some(token) = header.or(query) else {
            return false;
        };
        let Some(binding) = crate::probes::verify(token, &state.remote.key) else {
            return false;
        };
        if !binding.directive.safe(&inner.privacy)
            || !(binding.directive.service == "*"
                || binding.directive.service == inner.config.service)
            || !(binding.directive.environment == "*"
                || binding.directive.environment == inner.config.environment)
        {
            return false;
        }
        operation.bind_trigger(&inner.operations, Some(binding))
    }
    pub fn probe(&self, label: &str, value: &Value, options: ProbeOptions, context: &Context) {
        if crate::runtime::in_callback() {
            return;
        }
        let Some(inner) = &self.inner else {
            return;
        };
        if !valid_context(inner, context) || !valid_label(inner, label) {
            return;
        }
        let directives = {
            let Ok(mut state) = inner.state.try_lock() else {
                return;
            };
            if !eligible(inner, &state, label, options, context) {
                return;
            }
            let Some(data) = object(inner.privacy.protect(value)) else {
                return;
            };
            if !state.probes.insert(
                label,
                data.clone(),
                inner.config.max_probe_entries_per_label,
            ) {
                return;
            }
            matching(inner, &state, label, context)
                .into_iter()
                .map(|directive| (directive, data.clone()))
                .collect::<Vec<_>>()
        };
        self.emit_probes(label, directives, context);
    }
    pub fn probe_lazy<P>(
        &self,
        label: &str,
        producer: &Arc<P>,
        options: ProbeOptions,
        context: &Context,
    ) where
        P: Fn() -> Value + Send + Sync + 'static,
    {
        if crate::runtime::in_callback() {
            return;
        }
        let Some(inner) = &self.inner else {
            return;
        };
        if !inner.callbacks.available() {
            return;
        }
        if !valid_context(inner, context) || !valid_label(inner, label) {
            return;
        }
        {
            let Ok(mut state) = inner.state.try_lock() else {
                return;
            };
            if state.pending_probe.is_some()
                || !lazy_capacity(inner, &state)
                || !eligible(inner, &state, label, options, context)
            {
                return;
            }
            state.pending_probe = Some(label.into());
        }
        let inner = inner.clone();
        let label = label.to_owned();
        let producer = producer.clone();
        let context = context.clone();
        crate::runtime::spawn(async move {
            let worker = inner.clone();
            let worker_context = context.clone();
            let worker_label = label.clone();
            let callback_producer = producer.clone();
            let result = inner
                .callbacks
                .call(move || {
                    if !valid_context(&worker, &worker_context) {
                        return None;
                    }
                    {
                        let state = worker.state.lock().ok()?;
                        if !lazy_capacity(&worker, &state)
                            || !eligible(&worker, &state, &worker_label, options, &worker_context)
                        {
                            return None;
                        }
                    }
                    let raw = callback_producer();
                    let output = object(worker.privacy.protect(&raw));
                    crate::owned_json::discard(raw);
                    output
                })
                .await;
            // This task owns a second Arc so failed scheduling never drops the
            // application's last producer reference on the shared I/O runtime.
            // Pending admission and its client permit remain charged through
            // disposal, bounding this queue to one producer cleanup per client.
            crate::runtime::cleanup_value(producer, inner.slot.clone()).await;
            let directives = {
                let Ok(mut state) = inner.state.lock() else {
                    return;
                };
                if let Ok(Some(data)) = result {
                    if eligible(&inner, &state, &label, options, &context)
                        && state.probes.insert(
                            &label,
                            data.clone(),
                            inner.config.max_probe_entries_per_label,
                        )
                    {
                        matching(&inner, &state, &label, &context)
                            .into_iter()
                            .map(|directive| (directive, data.clone()))
                            .collect::<Vec<_>>()
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                }
            };
            {
                let client = Client::retained(inner.clone());
                client.emit_probes(&label, directives, &context);
            }
            if let Ok(mut state) = inner.state.lock() {
                state.pending_probe = None;
            }
            inner.changed.notify_waiters();
        });
    }
    fn emit_probes(&self, label: &str, directives: Vec<(Directive, Value)>, context: &Context) {
        for (directive, data) in directives {
            self.enqueue(0,|state|state.policy.probes_standalone,|inner,state|super::capture::base(inner,state,"probe_event",json!({"label":label,"data":data,"activation_id":directive.id,"probe_label_pattern":directive.pattern}),context));
        }
    }
}
fn object(value: Option<Value>) -> Option<Value> {
    let value = value?;
    Some(if value.is_object() {
        value
    } else {
        json!({"value":value})
    })
}
pub(super) fn valid_context(inner: &Inner, context: &Context) -> bool {
    !inner.stopped.load(Ordering::Acquire)
        && context
            .operation
            .as_ref()
            .is_none_or(|operation| operation.matches(&inner.operations))
        && crate::operations::current()
            .as_ref()
            .and_then(|current| current.operation.as_ref())
            .is_none_or(|operation| !operation.belongs(&inner.operations) || operation.is_active())
}
fn valid_label(inner: &Inner, label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 256
        && !label.chars().any(char::is_control)
        && inner.privacy.text(label) == label
}
fn matching(inner: &Inner, state: &State, label: &str, context: &Context) -> Vec<Directive> {
    let mut directives =
        state
            .remote
            .matching(label, &inner.config.service, &inner.config.environment);
    if state.remote.remote_enabled {
        if let Some(binding) = super::capture::effective(inner, context)
            .operation
            .and_then(|operation| operation.trigger(&inner.operations))
        {
            if binding.key_hash == crate::probes::key_hash(&state.remote.key)
                && binding.directive.matches(
                    label,
                    &inner.config.service,
                    &inner.config.environment,
                )
            {
                directives.push(binding.directive);
            }
        }
    }
    directives
}
fn eligible(
    inner: &Inner,
    state: &State,
    label: &str,
    options: ProbeOptions,
    context: &Context,
) -> bool {
    valid_context(inner, context)
        && state.remote.enabled
        && state.probes.can_label(
            label,
            inner.config.max_probe_labels,
            state.pending_probe.as_deref(),
        )
        && (!options.heavy || !matching(inner, state, label, context).is_empty())
}

// A lazy application producer is optional work. Recheck queue ownership both
// before scheduling and immediately before invocation, independently of whether
// native I/O currently leaves the application callback lane available.
fn lazy_capacity(inner: &Inner, state: &State) -> bool {
    state.owned < inner.config.max_buffered_events && state.bytes < inner.config.max_buffered_bytes
}

#[cfg(test)]
mod tests;
