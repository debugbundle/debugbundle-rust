use super::{now, Client, Entry, Inner, State};
use crate::{Context, DeploySnapshot, ExceptionSource, Level, RequestSnapshot};
use serde_json::{json, Value};
use std::{sync::atomic::Ordering, time::Instant};
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CaptureMode {
    Ordinary,
    PairedLog,
    Panic,
}

impl Client {
    pub fn capture_exception(&self, error: impl ExceptionSource, context: &Context) {
        self.enqueue(
            4,
            |_| true,
            |inner, state| {
                let snapshot = error.snapshot();
                exception(inner, state, &snapshot, context)
            },
        );
    }
    pub fn capture_error(&self, error: impl ExceptionSource, context: &Context) {
        self.capture_exception(error, context);
    }
    pub fn capture_log(&self, level: Level, message: &str, context: &Context) {
        if !self.log_enabled(level) {
            return;
        }
        self.enqueue(
            if level >= Level::Error { 3 } else { 1 },
            |state| {
                state.policy.log(
                    level,
                    self.inner
                        .as_ref()
                        .map_or(Level::Warning, |i| i.config.log_level),
                )
            },
            |inner, state| {
                let message = inner.privacy.text(if message.is_empty() {
                    "Log message unavailable"
                } else {
                    message
                });
                base(
                    inner,
                    state,
                    "log_event",
                    json!({"level":level.as_str(),"message":message,"attributes":{}}),
                    context,
                )
            },
        );
    }
    pub fn capture_message(&self, message: &str, level: Level, context: &Context) {
        self.capture_log(level, message, context);
    }
    pub fn capture_request(&self, request: &RequestSnapshot, context: &Context) {
        self.enqueue(if request.status>=400{3}else{1},|state|state.policy.request(request),|inner,state|{
            let(path,query)=request_path(inner,&request.path);
            let headers=inner.privacy.protect(&request.headers).filter(Value::is_object).unwrap_or_else(||json!({}));
            let body=request.body.as_ref().and_then(|v|inner.privacy.protect(v)).filter(|v|serde_json::to_vec(v).is_ok_and(|b|b.len()<=8192));
            let method=if request.method.is_empty()||request.method.len()>16{"UNKNOWN"}else{&request.method};
            let route=request.route_template.as_deref().map(|s|crate::model::text(s,2048));
            base(inner,state,"request_event",json!({"method":method,"path":path,"query":query,"headers":headers,"body":body,"response_status":request.status,"duration_ms":request.duration_ms,"route_template":route}),context)
        });
    }
    pub fn capture_deploy(&self, deploy: &DeploySnapshot) {
        self.enqueue(1,|_|true,|inner,state|base(inner,state,"deploy_metadata",json!({"commit_sha":inner.privacy.text(&deploy.commit_sha),"version":inner.privacy.text(&deploy.version),"branch":inner.privacy.text(&deploy.branch),"environment":inner.config.environment,"deployed_at":inner.privacy.text(&deploy.deployed_at)}),&Context::default()));
    }
    pub(super) fn enqueue(
        &self,
        priority: u8,
        eligible: impl FnOnce(&State) -> bool,
        build: impl FnOnce(&Inner, &State) -> Option<Value>,
    ) {
        self.enqueue_with_receipt(priority, eligible, build, CaptureMode::Ordinary, |_, _| {});
    }
    pub(super) fn enqueue_with_receipt(
        &self,
        priority: u8,
        eligible: impl FnOnce(&State) -> bool,
        build: impl FnOnce(&Inner, &State) -> Option<Value>,
        mode: CaptureMode,
        admitted: impl FnOnce(&mut State, &Value),
    ) {
        let Some(inner) = &self.inner else {
            return;
        };
        if inner.stopped.load(Ordering::Acquire)
            || crate::runtime::in_callback()
            || (super::panic::observing() && mode != CaptureMode::Panic)
        {
            return;
        }
        let Ok(mut state) = inner.state.try_lock() else {
            inner.record_pressure();
            return;
        };
        if !eligible(&state) {
            return;
        }
        if state.owned >= inner.config.max_buffered_events
            && !state.pending.iter().any(|entry| entry.priority < priority)
        {
            inner.record_pressure();
            return;
        }
        let Some(event) =
            build(inner, &state).and_then(|v| crate::wire::protect_event(&inner.privacy, &v))
        else {
            inner.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let reclaimable = state
            .pending
            .iter()
            .filter(|entry| entry.priority < priority)
            .fold(0usize, |sum, entry| sum.saturating_add(entry.bytes));
        let available = inner
            .config
            .max_buffered_bytes
            .saturating_sub(state.bytes.saturating_sub(reclaimable));
        let Some(event) = crate::wire::fit_event(&event, available) else {
            inner.record_pressure();
            return;
        };
        let Some(bytes) = crate::wire::native_size(&event, crate::wire::MAX_EVENT_BYTES) else {
            return;
        };
        if bytes > crate::wire::MAX_EVENT_BYTES || bytes > inner.config.max_buffered_bytes {
            inner.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        while state.owned >= inner.config.max_buffered_events
            || bytes > inner.config.max_buffered_bytes.saturating_sub(state.bytes)
        {
            let Some(index) = state
                .pending
                .iter()
                .position(|entry| entry.priority < priority)
            else {
                inner.record_pressure();
                return;
            };
            if let Some(removed) = state.pending.remove(index) {
                if let Some(id) = removed.event["event_id"].as_str() {
                    state.occurrences.revoke(id);
                }
                if let Some(receipt) = &removed.receipt {
                    state.diagnostics.release(receipt);
                }
                state.bytes = state.bytes.saturating_sub(removed.bytes);
                state.owned = state.owned.saturating_sub(1);
                inner.record_pressure();
            }
        }
        let finalized = mode == CaptureMode::PairedLog
            && inner.config.before_send.is_none()
            && inner.config.sample_rate == 1.0
            && event["event_type"] == "backend_exception";
        if finalized
            && !state.suppression.observe(
                &super::sender::fingerprint(&event),
                Instant::now(),
                event["occurred_at"].as_str().unwrap_or(""),
            )
        {
            inner.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let receipt = if event["event_type"] == "diagnostic_context" {
            if !inner.config.capture_recovery_observations
                || !state
                    .diagnostics
                    .reserve(&event, bytes, Instant::now(), chrono::Utc::now())
            {
                inner.dropped.fetch_add(1, Ordering::Relaxed);
                return;
            }
            event["event_id"].as_str().map(str::to_owned)
        } else {
            None
        };
        if event["event_type"] == "backend_exception"
            && event["payload"].get("probe_data").is_some()
        {
            state.probes.clear();
        }
        state.owned += 1;
        state.bytes += bytes;
        admitted(&mut state, &event);
        state.pending.push_back(Entry {
            event,
            bytes,
            priority,
            admitted: Instant::now(),
            finalized,
            hook_allowed: mode != CaptureMode::Panic,
            receipt,
        });
        if state.pending.len() >= inner.config.batch_size
            || state.flush_deadline.is_some()
            || state
                .pending
                .back()
                .is_some_and(|entry| entry.event["event_type"] == "probe_event")
        {
            inner.wake.notify_one();
        }
    }
}
pub(super) fn exception(
    inner: &Inner,
    state: &State,
    snapshot: &crate::ExceptionSnapshot,
    context: &Context,
) -> Option<Value> {
    let context = effective(inner, context);
    let mut payload = json!({"name":snapshot.name(),"message":snapshot.message(),"stack":snapshot.stack().unwrap_or("Stack unavailable"),"handled":true,
        "request":{"method":"UNKNOWN","path":"/","query":{},"headers":{}},"response":{"status_code":0},"runtime":{"version":option_env!("DEBUGBUNDLE_RUSTC_VERSION").unwrap_or("unknown"),"platform":std::env::consts::OS,"arch":std::env::consts::ARCH,"pid":std::process::id()}});
    #[cfg(any(feature = "tower", feature = "actix-web"))]
    if let Some(request) = &context.http {
        let (path, query) = request_path(inner, &request.snapshot.path);
        payload["request"] = json!({"method":request.snapshot.method,"path":path,"query":query,"headers":request.snapshot.headers});
        if let Some(body) = &request.snapshot.body {
            payload["request"]["body"] = body.clone();
        }
        payload["response"]["status_code"] = json!(request.status.load(Ordering::Acquire));
        if let Some(version) = &request.version {
            payload["runtime"]["framework_version"] = json!(version);
        }
    }
    if inner.config.capture_exception_evidence {
        payload["exception_evidence"] = snapshot.evidence().clone();
    }
    if inner.config.probe_flush_on_error {
        if let Some(probes) = state.probes.snapshot() {
            payload["probe_data"] = probes;
        }
    }
    if let Some(execution) = context
        .operation
        .as_ref()
        .and_then(|operation| operation.execution(&inner.operations))
    {
        payload["execution"] = execution;
    }
    base(inner, state, "backend_exception", payload, &context)
}
pub(super) fn base(
    inner: &Inner,
    state: &State,
    event_type: &str,
    payload: Value,
    context: &Context,
) -> Option<Value> {
    let context = effective(inner, context);
    let mut merged = state.context.as_object().cloned().unwrap_or_default();
    if let Some(values) = context.as_value().as_object() {
        for (key, value) in values {
            if merged.len() >= 256 {
                break;
            }
            merged.insert(key.clone(), value.clone());
        }
    }
    let event = envelope(
        inner,
        event_type,
        payload,
        Value::Object(merged),
        &context.correlation,
    );
    #[cfg(any(feature = "tower", feature = "actix-web"))]
    let event = event.map(|mut event| {
        if let Some(request) = &context.http {
            event["service"]["framework"] = json!(request.framework);
        }
        event
    });
    event
}
pub(super) fn envelope(
    inner: &Inner,
    event_type: &str,
    payload: Value,
    context: Value,
    correlation: &serde_json::Map<String, Value>,
) -> Option<Value> {
    let id = crate::identity::fresh()?;
    Some(
        json!({"schema_version":"2026-03-01","event_id":id,"event_type":event_type,"sdk_name":"@debugbundle/sdk-rust","sdk_version":env!("CARGO_PKG_VERSION"),"service":{"name":inner.config.service,"environment":inner.config.environment,"runtime":"rust"},"occurred_at":now(),"context":context,"correlation":correlation,"payload":payload}),
    )
}
pub(super) fn effective(inner: &Inner, context: &Context) -> Context {
    let current = crate::operations::current()
        .filter(|ctx| {
            ctx.operation
                .as_ref()
                .is_some_and(|operation| operation.matches(&inner.operations))
        })
        .unwrap_or_default();
    current.overlay(context, &inner.operations)
}
fn request_path(inner: &Inner, path: &str) -> (String, Value) {
    if path.len() > 16384 {
        return ("/[withheld]".into(), json!({}));
    }
    let absolute = if path.starts_with('/') {
        format!("https://request.invalid{path}")
    } else {
        path.to_owned()
    };
    if let Ok(url) = url::Url::parse(&absolute) {
        let mut query = serde_json::Map::new();
        for (key, value) in url.query_pairs().take(256) {
            query.insert(key.into_owned(), Value::String(value.into_owned()));
        }
        (
            inner.privacy.text(url.path()),
            inner
                .privacy
                .protect(&Value::Object(query))
                .filter(Value::is_object)
                .unwrap_or_else(|| json!({})),
        )
    } else {
        ("/[unavailable]".into(), json!({}))
    }
}
