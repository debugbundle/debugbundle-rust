//! One response trust boundary for native sender and relay forwarding.
use super::Inner;
use crate::{delivery::TransportResponse, wire};
use std::sync::atomic::Ordering;
pub(super) fn observe(inner: &Inner, response: &TransportResponse, native: bool, valid_ack: bool) {
    if let Ok(mut state) = inner.state.lock() {
        if native
            || !valid_ack
            || response
                .body
                .as_ref()
                .is_some_and(|body| body.get("evidence_capabilities").is_some())
        {
            state.features = if valid_ack {
                response
                    .body
                    .as_ref()
                    .map(|body| wire::capabilities(&body["evidence_capabilities"]))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
        }
        if valid_ack {
            if let Some(body) = &response.body {
                state.remote.piggyback(body, &inner.privacy);
                if body.get("capture_policy").is_some() {
                    if let Some(policy) = crate::policy::Policy::parse(body) {
                        inner.log_level.store(
                            policy
                                .logs
                                .map_or(u8::MAX, |level| level.max(inner.config.log_level) as u8),
                            Ordering::Release,
                        );
                        state.policy = policy;
                    }
                }
            }
        }
    }
}
