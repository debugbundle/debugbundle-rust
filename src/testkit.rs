//! Explicit bounded in-memory delivery for application tests; no global install.
use crate::{
    delivery::{Transport, TransportFailure, TransportResponse},
    privacy::Privacy,
    wire,
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicU8, AtomicUsize, Ordering},
        Mutex,
    },
    time::Duration,
};
const MAX_BYTES: usize = 8 * 1024 * 1024;
/// Numeric test instrumentation only; no event payloads, identities or credentials.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceSnapshot {
    pub buffered_events: usize,
    pub buffered_bytes: usize,
    pub diagnostic_bytes: usize,
    pub diagnostic_windows: usize,
    pub pending_diagnostics: usize,
    pub operation_slots: usize,
    pub operation_metadata_bytes: usize,
    pub process_client_slots: usize,
    pub process_callback_workers: usize,
}
/// Non-waiting observation. Returns None for inactive clients or contended state;
/// process-wide counters may advance independently of this client's snapshot.
pub fn resources(client: &crate::Client) -> Option<ResourceSnapshot> {
    client.test_resources()
}
struct State {
    events: Vec<Value>,
    bytes: usize,
}
pub struct RecordingTransport {
    capacity: usize,
    state: Mutex<State>,
    attempts: AtomicUsize,
    failures: AtomicU8,
    privacy: Privacy,
}
impl RecordingTransport {
    fn should_fail(&self) -> bool {
        for _ in 0..32 {
            let failures = self.failures.load(Ordering::Acquire);
            if failures == 0 {
                return false;
            }
            if self
                .failures
                .compare_exchange_weak(failures, failures - 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
        true
    }
    /// Between one and 1,000 events, sharing a fixed eight-MiB retained-byte cap.
    pub fn new(capacity: usize) -> Option<Self> {
        if !(1..=1000).contains(&capacity) {
            return None;
        }
        Some(Self {
            capacity,
            state: Mutex::new(State {
                events: Vec::new(),
                bytes: 0,
            }),
            attempts: AtomicUsize::new(0),
            failures: AtomicU8::new(0),
            privacy: Privacy::new(&[])?,
        })
    }
    pub fn fail_next(&self, attempts: u8) {
        self.failures.store(attempts, Ordering::Release);
    }
    pub fn attempts(&self) -> usize {
        self.attempts.load(Ordering::Acquire)
    }
    pub fn events(&self) -> Vec<Value> {
        self.state
            .lock()
            .map(|state| state.events.clone())
            .unwrap_or_default()
    }
    pub fn drain(&self) -> Vec<Value> {
        self.state
            .lock()
            .map(|mut state| {
                state.bytes = 0;
                std::mem::take(&mut state.events)
            })
            .unwrap_or_default()
    }
}
impl Transport for RecordingTransport {
    fn send(&self, body: &[u8], _: Duration) -> Result<TransportResponse, TransportFailure> {
        self.attempts.fetch_add(1, Ordering::AcqRel);
        if self.should_fail() || body.len() > wire::MAX_BODY_BYTES {
            return Err(TransportFailure);
        }
        let input: Value = serde_json::from_slice(body).map_err(|_| TransportFailure)?;
        if input.as_object().is_none_or(|root| root.len() != 1) {
            return Err(TransportFailure);
        }
        let input = input["events"]
            .as_array()
            .filter(|events| !events.is_empty() && events.len() <= 100)
            .ok_or(TransportFailure)?;
        let events = input
            .iter()
            .map(|event| {
                if event["event_type"] == "analytics_event" {
                    crate::analytics_wire::protect_event(&self.privacy, event)
                } else {
                    wire::protect_event(&self.privacy, event)
                }
            })
            .collect::<Option<Vec<_>>>()
            .ok_or(TransportFailure)?;
        let bytes = events
            .iter()
            .try_fold(0usize, |sum, event| {
                sum.checked_add(wire::native_size(event, wire::MAX_EVENT_BYTES)?)
            })
            .ok_or(TransportFailure)?;
        let count = events.len();
        let mut state = self.state.lock().map_err(|_| TransportFailure)?;
        if count > self.capacity.saturating_sub(state.events.len())
            || bytes > MAX_BYTES.saturating_sub(state.bytes)
        {
            return Err(TransportFailure);
        }
        state.bytes += bytes;
        state.events.extend(events);
        Ok(TransportResponse {
            status: 202,
            body: Some(json!({"accepted":count,"rejected":0,"errors":[]})),
            retry_after: None,
        })
    }
}
