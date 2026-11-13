use super::{sender, Client};
use crate::{config::ProjectMode, privacy::Privacy, protocol, wire};
use serde_json::{json, Value};
use std::{sync::atomic::Ordering, time::Instant};
impl Client {
    pub(crate) fn relay_budget(&self) -> Option<std::sync::Arc<crate::relay::IngressBudget>> {
        self.inner
            .as_ref()
            .filter(|inner| !inner.stopped.load(Ordering::Acquire))
            .map(|inner| inner.relay_budget.clone())
    }
    pub(crate) async fn relay_rate_limit(
        &self,
        peer: Option<std::net::IpAddr>,
        limit_per_minute: u32,
    ) -> Result<bool, ()> {
        let inner = self.inner.as_ref().ok_or(())?;
        let Some(store) = inner.config.relay_rate_limit_store.clone() else {
            return Ok(true);
        };
        let key = crate::relay::RateLimitKey {
            namespace: super::spool::scope(inner),
            peer,
            limit_per_minute,
        };
        match tokio::time::timeout(
            inner.config.request_timeout,
            inner.callbacks.call(move || store.allow(key)),
        )
        .await
        {
            Ok(Ok(Ok(allowed))) => Ok(allowed),
            _ => Err(()),
        }
    }
    pub(crate) fn relay_timeout(&self) -> std::time::Duration {
        self.inner
            .as_ref()
            .map_or(std::time::Duration::ZERO, |inner| {
                inner.config.request_timeout
            })
    }
    pub(crate) async fn relay_finalize(
        &self,
        event: Value,
        deadline: Instant,
    ) -> Result<Option<Value>, ()> {
        let inner = self
            .inner
            .as_ref()
            .filter(|inner| !inner.stopped.load(Ordering::Acquire))
            .ok_or(())?;
        let Some(hook) = inner.config.before_send.clone() else {
            return Ok(Some(event));
        };
        let original = event.clone();
        let privacy = inner.privacy.clone();
        match tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            inner.callbacks.call(move || {
                let replacement = hook(original.clone())?;
                if replacement.get("event_type").and_then(Value::as_str) == Some("analytics_event")
                {
                    let result = if !wire::preflight(&replacement) {
                        None
                    } else if wire::valid_analytics_event(&replacement) {
                        crate::analytics_wire::protect_event(&privacy, &replacement)
                    } else {
                        Some(original)
                    };
                    crate::owned_json::discard(replacement);
                    result
                } else {
                    wire::hook_replacement(&privacy, replacement, original)
                }
            }),
        )
        .await
        {
            Ok(Ok(event)) => Ok(event),
            Ok(Err(crate::runtime::CallbackFailure::Panicked)) => Ok(Some(event)),
            _ => Err(()),
        }
    }
    pub(crate) fn relay_privacy(&self) -> Option<Privacy> {
        self.inner
            .as_ref()
            .filter(|inner| !inner.stopped.load(Ordering::Acquire))
            .map(|inner| inner.privacy.clone())
    }
    pub(crate) fn relay_live(&self) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|inner| !inner.stopped.load(Ordering::Acquire))
    }
    pub(crate) fn relay_features(&self) -> Vec<String> {
        self.inner
            .as_ref()
            .filter(|inner| !inner.stopped.load(Ordering::Acquire))
            .and_then(|inner| inner.state.try_lock().ok())
            .map(|state| state.features.clone())
            .unwrap_or_default()
    }
    pub(crate) async fn relay_deliver(&self, events: Vec<Value>, durable: bool) -> bool {
        let Some(inner) = self
            .inner
            .as_ref()
            .filter(|inner| !inner.stopped.load(Ordering::Acquire))
        else {
            return false;
        };
        if inner.config.project_mode == ProjectMode::Connected && durable {
            let directory = inner.config.spool_dir.clone();
            let scope = super::spool::scope(inner);
            let features = self.relay_features();
            let admission = super::spool::Admission::new(inner.clone());
            let retained = tokio::time::timeout(
                inner.config.request_timeout,
                inner.callbacks.call(move || {
                    let _admission = admission;
                    crate::relay::spool::Store::open(&directory)?.admit(
                        events,
                        &scope,
                        &features,
                        chrono::Utc::now().timestamp_millis(),
                    )
                }),
            )
            .await;
            if !matches!(retained, Ok(Ok(Ok(_)))) {
                return false;
            }
            self.start_relay_spool();
            inner.spool_wake.notify_one();
            return true;
        }
        let deadline = Instant::now() + inner.config.request_timeout;
        let mut offset = 0;
        while offset < events.len() {
            // A preceding chunk or config response may revoke reader proof.
            // Keep original IDs and apply the current bound to every unsent tail.
            let features = self.relay_features();
            let Some(remaining) = events[offset..]
                .iter()
                .map(|event| {
                    crate::relay::validation::for_delivery(event, &inner.privacy, &features)
                })
                .collect::<Option<Vec<_>>>()
            else {
                return false;
            };
            let mut bytes = 13;
            let mut count = 0;
            for event in &remaining {
                let Some(size) = serde_json::to_vec(event).ok().map(|value| value.len()) else {
                    return false;
                };
                let extra = size + usize::from(count > 0);
                if extra > wire::MAX_BODY_BYTES.saturating_sub(bytes) {
                    break;
                }
                bytes += extra;
                count += 1;
                if count == 100 {
                    break;
                }
            }
            if count == 0 {
                return false;
            }
            let chunk = remaining[..count].to_vec();
            let Ok(body) = serde_json::to_vec(&json!({"events":chunk})) else {
                return false;
            };
            let Ok(Some((response, native, requires))) = tokio::time::timeout(
                deadline.saturating_duration_since(Instant::now()),
                sender::send(inner, body, chunk, deadline),
            )
            .await
            else {
                return false;
            };
            let ack = if (200..300).contains(&response.status) {
                response
                    .body
                    .as_ref()
                    .and_then(|body| protocol::acknowledgement(body, count))
            } else {
                None
            };
            let legacy =
                (200..300).contains(&response.status) && response.body.is_none() && !requires;
            super::response::observe(inner, &response, native, ack.is_some() || legacy);
            let complete = ack.is_some_and(|ack| ack.accepted.len() == count) || legacy;
            if !complete {
                return false;
            }
            offset += count;
        }
        true
    }
}
