use debugbundle::{
    delivery::{Transport, TransportFailure, TransportResponse},
    Config,
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
#[derive(Default)]
pub struct Recorder {
    pub events: Mutex<Vec<Value>>,
    pub calls: AtomicUsize,
    pub failures: AtomicUsize,
}
impl Recorder {
    // Contended capture can emit this canonical operational summary even when
    // no requested primary/diagnostic was admitted. Keep every other event.
    #[allow(dead_code)]
    pub fn events_without_pressure(&self) -> Vec<Value> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| {
                !(event["event_type"] == "error_suppressed"
                    && event["payload"]["reason"] == "queue_pressure")
            })
            .cloned()
            .collect()
    }
}

#[allow(dead_code)]
pub fn emit_until(
    client: &debugbundle::Client,
    mut emit: impl FnMut(),
    observed: impl Fn() -> bool,
) -> usize {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut attempts = 0;
    loop {
        // Observe required records separately: draining does not reserve the
        // state mutex for a subsequent nonblocking best-effort capture call.
        emit();
        attempts += 1;
        assert!(client.flush(Duration::from_secs(2)).drained);
        if observed() {
            return attempts;
        }
        assert!(Instant::now() < deadline, "native record never admitted");
        std::thread::yield_now();
    }
}
impl Transport for Recorder {
    fn send(&self, body: &[u8], _: Duration) -> Result<TransportResponse, TransportFailure> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self
            .failures
            .compare_exchange(1, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return Ok(TransportResponse {
                status: 202,
                body: Some(json!({"accepted":999,"rejected":0,"errors":[]})),
                retry_after: Some(Duration::from_millis(1)),
            });
        }
        let parsed: Value = serde_json::from_slice(body).unwrap();
        let events = parsed["events"].as_array().unwrap();
        self.events.lock().unwrap().extend(events.iter().cloned());
        Ok(TransportResponse {
            status: 202,
            body: Some(json!({"accepted":events.len(),"rejected":0,"errors":[]})),
            retry_after: None,
        })
    }
}
pub fn configured(recorder: Arc<Recorder>) -> Config {
    Config {
        transport: Some(recorder),
        batch_size: 100,
        flush_interval: Duration::from_secs(5),
        ..Config::new("synthetic", "checkout")
    }
}
