//! One sender per client; ownership survives hook, byte packing, ACK and retry.
use super::{Entry, Inner};
use crate::{config::ProjectMode, delivery::TransportResponse, protocol, wire};
use serde_json::{json, Value};
use std::{
    sync::{atomic::Ordering, Arc, Weak},
    time::{Duration, Instant, SystemTime},
};

pub(super) async fn run(weak: Weak<Inner>) {
    loop {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        if inner.stopped.load(Ordering::Acquire) {
            clear(&inner);
            return;
        }
        let delay = inner
            .state
            .lock()
            .ok()
            .map(|s| {
                if s.failures > 0 && !s.pending.is_empty() {
                    s.next_attempt
                        .saturating_duration_since(Instant::now())
                        .min(inner.config.flush_interval)
                } else {
                    inner.config.flush_interval
                }
            })
            .unwrap_or(Duration::from_secs(5));
        tokio::select! {_ = inner.wake.notified()=>{},_ = tokio::time::sleep(delay)=>{}}
        if inner.stopped.load(Ordering::Acquire) {
            clear(&inner);
            return;
        }
        drain(&inner).await;
    }
}
async fn drain(inner: &Arc<Inner>) {
    let deadline = inner
        .state
        .lock()
        .ok()
        .and_then(|s| s.flush_deadline)
        .filter(|deadline| *deadline > Instant::now())
        .unwrap_or_else(|| Instant::now() + inner.config.request_timeout);
    loop {
        if inner.stopped.load(Ordering::Acquire) || Instant::now() >= deadline {
            break;
        }
        operational_aggregates(inner);
        let batch = {
            let Ok(mut state) = inner.state.lock() else {
                return;
            };
            if state.next_attempt > Instant::now() {
                return;
            }
            let count = state.pending.len().min(inner.config.batch_size).min(100);
            state.pending.drain(..count).collect::<Vec<_>>()
        };
        if batch.is_empty() {
            break;
        }
        let mut finalized = Vec::new();
        for mut entry in batch {
            if entry.admitted.elapsed() >= inner.config.max_event_age {
                release(inner, entry, true);
                continue;
            }
            if !entry.finalized {
                if let Some(hook) = inner
                    .config
                    .before_send
                    .clone()
                    .filter(|_| entry.hook_allowed)
                {
                    let original = entry.event.clone();
                    let privacy = inner.privacy.clone();
                    let result = inner
                        .callbacks
                        .call(move || match hook(original.clone()) {
                            None => None,
                            Some(replacement) => {
                                wire::hook_replacement(&privacy, replacement, original)
                            }
                        })
                        .await;
                    match result {
                        Ok(Some(event)) => entry.event = event,
                        Ok(None) => {
                            release(inner, entry, true);
                            continue;
                        }
                        Err(crate::runtime::CallbackFailure::Panicked) => {}
                        Err(crate::runtime::CallbackFailure::Unavailable) => {
                            release(inner, entry, true);
                            continue;
                        }
                    }
                }
                entry.finalized = true;
                if !inner.callbacks.available()
                    && (inner.config.transport.is_some()
                        || inner.config.project_mode == ProjectMode::LocalOnly)
                {
                    release(inner, entry, true);
                    continue;
                }
                if entry.event["event_type"] != "error_suppressed"
                    && !sampled(&entry.event, inner.config.sample_rate)
                {
                    release(inner, entry, true);
                    continue;
                }
                if [
                    "backend_exception",
                    "frontend_exception",
                    "log_event",
                    "request_event",
                ]
                .iter()
                .any(|kind| entry.event["event_type"] == *kind)
                {
                    let key = fingerprint(&entry.event);
                    let timestamp = entry.event["occurred_at"].as_str().unwrap_or("");
                    let admitted = inner.state.lock().ok().is_some_and(|mut state| {
                        state.suppression.observe(&key, Instant::now(), timestamp)
                    });
                    if !admitted {
                        release(inner, entry, true);
                        continue;
                    }
                }
            }
            let Some(event) = filtered(inner, &entry.event) else {
                release(inner, entry, true);
                continue;
            };
            entry.event = event;
            if entry.admitted.elapsed() >= inner.config.max_event_age
                || inner.stopped.load(Ordering::Acquire)
            {
                release(inner, entry, true);
                continue;
            }
            let fitted = if let Ok(mut state) = inner.state.lock() {
                let other = state.bytes.saturating_sub(entry.bytes);
                wire::fit_event(
                    &entry.event,
                    inner.config.max_buffered_bytes.saturating_sub(other),
                )
                .and_then(|event| {
                    let bytes = wire::native_size(&event, wire::MAX_EVENT_BYTES)?;
                    state.bytes = other + bytes;
                    Some((event, bytes))
                })
            } else {
                None
            };
            let Some((event, bytes)) = fitted else {
                release(inner, entry, true);
                continue;
            };
            entry.event = event;
            entry.bytes = bytes;
            finalized.push(entry);
        }
        while !finalized.is_empty() {
            if Instant::now() >= deadline || inner.stopped.load(Ordering::Acquire) {
                requeue(inner, finalized);
                return;
            }
            // A preceding chunk may revoke proof or tighten policy. Recheck
            // every unsent tail without running its application hook again.
            finalized = finalized
                .into_iter()
                .filter_map(|entry| refilter(inner, entry))
                .collect();
            if finalized.is_empty() {
                break;
            }
            let mut size = 13usize;
            let mut count = 0;
            for entry in &finalized {
                let extra = entry.bytes + usize::from(count > 0);
                if extra > wire::MAX_BODY_BYTES.saturating_sub(size) {
                    break;
                }
                size += extra;
                count += 1;
            }
            if count == 0 {
                let entry = finalized.remove(0);
                release(inner, entry, true);
                continue;
            }
            let chunk = finalized.drain(..count).collect::<Vec<_>>();
            let events = chunk
                .iter()
                .map(|entry| entry.event.clone())
                .collect::<Vec<_>>();
            let Ok(body) = serde_json::to_vec(&json!({"events":events})) else {
                for entry in chunk {
                    release(inner, entry, true);
                }
                continue;
            };
            let response = send(inner, body, events, deadline).await;
            let mut retry = Vec::new();
            match response {
                Some((response, native, requires_ack)) => {
                    let ack = if (200..300).contains(&response.status) {
                        match &response.body {
                            Some(body) => protocol::acknowledgement(body, chunk.len()),
                            None if !requires_ack => Some(protocol::Acknowledgement {
                                accepted: (0..chunk.len()).collect(),
                                retry: Vec::new(),
                                terminal: Vec::new(),
                            }),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    super::response::observe(inner, &response, native, ack.is_some());
                    if let Some(ack) = ack {
                        let has_retry = !ack.retry.is_empty();
                        for (index, entry) in chunk.into_iter().enumerate() {
                            if ack.accepted.contains(&index) {
                                inner.accepted.fetch_add(1, Ordering::Relaxed);
                                inner.last_event.store(
                                    SystemTime::now()
                                        .duration_since(SystemTime::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_millis()
                                        .min(u128::from(u64::MAX))
                                        as u64,
                                    Ordering::Relaxed,
                                );
                                release(inner, entry, false);
                            } else if ack.retry.contains(&index) {
                                retry.push(entry);
                            } else {
                                release(inner, entry, true);
                            }
                        }
                        if let Ok(mut state) = inner.state.lock() {
                            state.failures = if has_retry {
                                state.failures.saturating_add(1)
                            } else {
                                0
                            };
                            state.rate_limited = has_retry;
                            state.next_attempt = Instant::now()
                                + response
                                    .retry_after
                                    .unwrap_or(if has_retry {
                                        Duration::from_millis(100)
                                    } else {
                                        Duration::ZERO
                                    })
                                    .min(Duration::from_secs(300));
                        }
                    } else {
                        retry = chunk;
                        backoff(inner, response.status == 429, response.retry_after);
                    }
                }
                None => {
                    retry = chunk;
                    backoff(inner, false, None);
                }
            }
            if !retry.is_empty() {
                retry.extend(finalized);
                requeue(inner, retry);
                return;
            }
        }
    }
    if let Ok(mut state) = inner.state.lock() {
        if state.pending_probe.is_none() {
            state.flush_deadline = None;
        }
    }
}
pub(super) async fn send(
    inner: &Arc<Inner>,
    body: Vec<u8>,
    events: Vec<Value>,
    deadline: Instant,
) -> Option<(TransportResponse, bool, bool)> {
    let timeout = deadline
        .saturating_duration_since(Instant::now())
        .min(inner.config.request_timeout);
    if timeout.is_zero() {
        return None;
    }
    if let Some(transport) = inner.config.transport.clone() {
        return inner
            .callbacks
            .call(move || {
                let requires = transport.requires_acknowledgement();
                let mut response = transport.send(&body, timeout).ok()?;
                if response.body.as_ref().is_some_and(|v| !wire::preflight(v)) {
                    if let Some(body) = response.body.take() {
                        crate::owned_json::discard(body);
                    }
                    return None;
                }
                Some((
                    TransportResponse {
                        retry_after: response
                            .retry_after
                            .map(|d| d.min(Duration::from_secs(300))),
                        ..response
                    },
                    false,
                    requires,
                ))
            })
            .await
            .ok()
            .flatten();
    }
    if inner.config.project_mode == ProjectMode::LocalOnly {
        #[cfg(feature = "local")]
        {
            let directory = inner.config.local_events_dir.clone();
            let service = crate::storage::filename_service(
                events
                    .first()
                    .and_then(|event| event["service"]["name"].as_str())
                    .unwrap_or(&inner.config.service),
            );
            let count = events.len();
            return inner
                .callbacks
                .call(move || {
                    let directory = crate::storage::SecureDirectory::open(&directory).ok()?;
                    directory.write_events(&events, &service).ok()?;
                    Some((
                        TransportResponse {
                            status: 202,
                            body: Some(json!({"accepted":count,"rejected":0,"errors":[]})),
                            retry_after: None,
                        },
                        false,
                        true,
                    ))
                })
                .await
                .ok()
                .flatten();
        }
        #[cfg(not(feature = "local"))]
        {
            let _ = events;
            return None;
        }
    }
    #[cfg(feature = "http")]
    if let Some(http) = &inner.http {
        let stopped = inner.stop.notified();
        tokio::pin!(stopped);
        stopped.as_mut().enable();
        if inner.stopped.load(Ordering::Acquire) {
            return None;
        }
        let response = tokio::select!{_ = stopped=>return None,response=tokio::time::timeout(timeout,http.send(body))=>response}.ok()?.ok()?;
        return Some((
            TransportResponse {
                status: response.status,
                body: Some(response.body),
                retry_after: response
                    .retry_at
                    .map(|at| at.saturating_duration_since(Instant::now())),
            },
            true,
            true,
        ));
    }
    None
}
fn filtered(inner: &Inner, event: &Value) -> Option<Value> {
    let (policy, features) = {
        let state = inner.state.lock().ok()?;
        (state.policy.clone(), state.features.clone())
    };
    if !policy.allows(event, inner.config.log_level) {
        return None;
    }
    let mut output = event.clone();
    let supported = |feature: &str| features.iter().any(|s| s == feature);
    if output["event_type"] == "diagnostic_context" {
        if output["payload"]["kind"] == "recovery" && !inner.config.capture_recovery_observations {
            return None;
        }
        let feature = if output["payload"]["kind"] == "recovery" {
            "recovery_v1"
        } else {
            "resource_observations_v1"
        };
        if !supported(feature) {
            return None;
        }
    }
    if let Some(payload) = output["payload"].as_object_mut() {
        if !supported("exception_evidence_v1") || !inner.config.capture_exception_evidence {
            payload.remove("exception_evidence");
            payload.remove("execution");
        }
        if !supported("resource_observations_v1") {
            payload.remove("resource_observations");
        }
    }
    if !supported("evidence_provenance_v1")
        && wire::valid_reference(&output["context"]["evidence_reference"])
    {
        if let Some(context) = output["context"].as_object_mut() {
            context.remove("evidence_reference");
        }
    }
    wire::protect_event(&inner.privacy, &output)
}
fn refilter(inner: &Inner, mut entry: Entry) -> Option<Entry> {
    let Some(event) = filtered(inner, &entry.event) else {
        release(inner, entry, true);
        return None;
    };
    let Some(bytes) = serde_json::to_vec(&event).ok().map(|bytes| bytes.len()) else {
        release(inner, entry, true);
        return None;
    };
    let permitted = inner.state.lock().ok().is_some_and(|mut state| {
        let other = state.bytes.saturating_sub(entry.bytes);
        if bytes > wire::MAX_EVENT_BYTES
            || bytes > inner.config.max_buffered_bytes.saturating_sub(other)
        {
            return false;
        }
        if event["event_type"] == "diagnostic_context" {
            if !state.diagnostics.replace(
                entry.receipt.as_deref(),
                &event,
                bytes,
                Instant::now(),
                chrono::Utc::now(),
            ) {
                return false;
            }
            entry.receipt = event["event_id"].as_str().map(str::to_owned);
        } else {
            if let Some(receipt) = entry.receipt.take() {
                state.diagnostics.release(&receipt);
            }
            if inner.config.capture_recovery_observations {
                state
                    .diagnostics
                    .register(&event, Instant::now(), chrono::Utc::now());
            }
        }
        state.bytes = other + bytes;
        true
    });
    if !permitted {
        release(inner, entry, true);
        return None;
    }
    entry.priority = match event["event_type"].as_str() {
        Some("backend_exception" | "frontend_exception") => 4,
        Some("diagnostic_context" | "probe_event") => 0,
        Some("log_event") => {
            if event["payload"]["level"]
                .as_str()
                .and_then(crate::Level::parse)
                .is_some_and(|level| level >= crate::Level::Error)
            {
                3
            } else {
                1
            }
        }
        Some("request_event") => {
            if wire::unsigned(&event["payload"]["response_status"])
                .is_some_and(|status| status >= 400)
            {
                3
            } else {
                1
            }
        }
        Some("error_suppressed") => 2,
        _ => 1,
    };
    entry.event = event;
    entry.bytes = bytes;
    Some(entry)
}
fn sampled(event: &Value, rate: f64) -> bool {
    if rate >= 1.0 {
        return true;
    }
    if rate <= 0.0 {
        return false;
    }
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(event["event_id"].as_str().unwrap_or("").as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&hash[..8]);
    (u64::from_be_bytes(bytes) as f64) / (u64::MAX as f64) < rate
}
pub(super) fn fingerprint(event: &Value) -> String {
    use sha2::{Digest, Sha256};
    let p = &event["payload"];
    let fields = json!([
        event["event_type"],
        p["name"],
        p["message"],
        p["method"],
        p["path"],
        p["response_status"],
        p["request"]["method"],
        p["request"]["path"]
    ]);
    let bytes = serde_json::to_vec(&fields).unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}
fn operational_aggregates(inner: &Inner) {
    let Ok(mut state) = inner.state.lock() else {
        return;
    };
    let pending = inner.pressure.load(Ordering::Acquire);
    if pending > 0 && state.owned < inner.config.max_buffered_events {
        let stamp = |n: u64| {
            chrono::DateTime::from_timestamp_millis(n.min(i64::MAX as u64) as i64)
                .unwrap_or_else(chrono::Utc::now)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        };
        let payload = json!({"fingerprint":"sdk-queue-pressure","suppressed_count":pending,"window_seconds":30,"first_seen":stamp(inner.pressure_first.load(Ordering::Acquire)),"last_seen":stamp(inner.pressure_last.load(Ordering::Acquire)),"reason":"queue_pressure","level":"error"});
        if let Some(mut event) = super::capture::base(
            inner,
            &state,
            "error_suppressed",
            payload,
            &crate::Context::default(),
        ) {
            if let Some(root) = event.as_object_mut() {
                root.remove("context");
            }
            if let Some(event) = wire::protect_event(&inner.privacy, &event) {
                if let Some(bytes) = serde_json::to_vec(&event)
                    .ok()
                    .map(|b| b.len())
                    .filter(|n| *n <= inner.config.max_buffered_bytes.saturating_sub(state.bytes))
                {
                    state.owned += 1;
                    state.bytes += bytes;
                    state.pending.push_back(Entry {
                        event,
                        bytes,
                        priority: 2,
                        admitted: Instant::now(),
                        finalized: false,
                        hook_allowed: true,
                        receipt: None,
                    });
                    inner.pressure.fetch_sub(pending, Ordering::AcqRel);
                    inner.pressure_first.store(0, Ordering::Release);
                }
            }
        }
    }
    let free = inner
        .config
        .max_buffered_events
        .saturating_sub(state.owned)
        .min(inner.config.max_buffered_bytes.saturating_sub(state.bytes) / 2048)
        .min(16);
    for aggregate in state.suppression.collect(Instant::now(), free) {
        let payload = json!({"fingerprint":aggregate.fingerprint,"suppressed_count":aggregate.count,"window_seconds":30,"first_seen":aggregate.first_seen,"last_seen":aggregate.last_seen});
        let Some(event) = super::capture::base(
            inner,
            &state,
            "error_suppressed",
            payload,
            &crate::Context::default(),
        )
        .and_then(|event| wire::protect_event(&inner.privacy, &event)) else {
            continue;
        };
        let Some(bytes) = serde_json::to_vec(&event).ok().map(|b| b.len()) else {
            continue;
        };
        if bytes > inner.config.max_buffered_bytes.saturating_sub(state.bytes) {
            continue;
        }
        state.owned += 1;
        state.bytes += bytes;
        state.pending.push_back(Entry {
            event,
            bytes,
            priority: 2,
            admitted: Instant::now(),
            finalized: false,
            hook_allowed: true,
            receipt: None,
        });
    }
}
fn release(inner: &Inner, mut entry: Entry, dropped: bool) {
    let bytes = entry.bytes;
    let receipt = entry.receipt.take();
    let event_id = dropped.then(|| entry.event["event_id"].as_str().unwrap_or("").to_owned());
    drop(entry);
    if let Ok(mut state) = inner.state.lock() {
        if let Some(id) = event_id {
            state.occurrences.revoke(&id);
        }
        if let Some(receipt) = receipt {
            state.diagnostics.release(&receipt);
        }
        state.owned = state.owned.saturating_sub(1);
        state.bytes = state.bytes.saturating_sub(bytes);
    }
    if dropped {
        inner.dropped.fetch_add(1, Ordering::Relaxed);
    }
    inner.changed.notify_waiters();
}
fn requeue(inner: &Inner, entries: Vec<Entry>) {
    if let Ok(mut state) = inner.state.lock() {
        for entry in entries.into_iter().rev() {
            state.pending.push_front(entry);
        }
    }
    inner.changed.notify_waiters();
}
fn backoff(inner: &Inner, rate_limited: bool, hint: Option<Duration>) {
    if let Ok(mut state) = inner.state.lock() {
        state.failures = state.failures.saturating_add(1);
        state.rate_limited = rate_limited;
        let multiplier = 1u64 << state.failures.min(8);
        let base = Duration::from_millis(100u64.saturating_mul(multiplier));
        let delay = hint.unwrap_or(base).min(Duration::from_secs(300));
        state.next_attempt = Instant::now() + delay;
    }
}
fn clear(inner: &Inner) {
    if let Ok(mut state) = inner.state.lock() {
        state.pending.clear();
        state.owned = 0;
        state.bytes = 0;
        state.diagnostics.clear();
        state.occurrences.clear();
    }
    inner.pressure.store(0, Ordering::Release);
    inner.changed.notify_waiters();
}
