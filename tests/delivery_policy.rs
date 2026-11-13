use debugbundle::{
    delivery::{Transport, TransportFailure, TransportResponse},
    Client, Config, Context, Level,
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

#[derive(Default)]
struct Destination {
    malformed: AtomicBool,
    accepted: Mutex<Vec<Value>>,
}
impl Transport for Destination {
    fn send(&self, bytes: &[u8], _: Duration) -> Result<TransportResponse, TransportFailure> {
        let batch: Value = serde_json::from_slice(bytes).unwrap();
        let events = batch["events"].as_array().unwrap();
        let malformed = self.malformed.load(Ordering::Acquire);
        if !malformed {
            self.accepted.lock().unwrap().extend(events.iter().cloned());
        }
        Ok(TransportResponse {
            status: 202,
            body: Some(json!({
                "accepted": if malformed { 999 } else { events.len() }, "rejected":0, "errors":[],
                "capture_policy": {"preset":"minimal", "capture_logs":"off", "capture_request_events":"off", "capture_probe_events":"buffer_only", "capture_diagnostic_context":false}
            })),
            // Keep one malformed reply in the first 40 ms observation window.
            // Rapid repeated failures exercise exponential backoff instead of
            // the policy/ownership boundary this fixture is checking.
            retry_after: Some(Duration::from_millis(100)),
        })
    }
}

#[test]
fn acknowledged_ingestion_policy_tightens_later_native_log_admission() {
    let destination = Arc::new(Destination::default());
    let client = Client::new(Config {
        transport: Some(destination.clone()),
        ..Config::new("synthetic", "checkout")
    });
    assert!(client.log_enabled(Level::Error));
    client.capture_log(Level::Error, "first", &Context::default());
    assert_eq!(client.flush(Duration::from_secs(2)).accepted_events, 1);
    assert!(!client.log_enabled(Level::Error));
    client.capture_log(
        Level::Critical,
        "disabled by authoritative policy",
        &Context::default(),
    );
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert_eq!(destination.accepted.lock().unwrap().len(), 1);
}

#[test]
fn malformed_ack_cannot_apply_piggyback_policy_or_release_pending_ownership() {
    let destination = Arc::new(Destination::default());
    destination.malformed.store(true, Ordering::Release);
    let client = Client::new(Config {
        transport: Some(destination.clone()),
        ..Config::new("synthetic", "checkout")
    });
    client.capture_log(Level::Error, "retained", &Context::default());
    let first = client.flush(Duration::from_millis(40));
    assert!(!first.drained);
    assert_eq!(first.accepted_events, 0);
    assert!(client.log_enabled(Level::Error));
    assert_eq!(client.status().buffered_events, 1);
    destination.malformed.store(false, Ordering::Release);
    assert_eq!(client.flush(Duration::from_secs(2)).accepted_events, 1);
    assert!(!client.log_enabled(Level::Error));
    assert_eq!(destination.accepted.lock().unwrap().len(), 1);
}
