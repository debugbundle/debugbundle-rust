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
    time::Duration,
};
#[derive(Default)]
pub struct Recorder {
    pub events: Mutex<Vec<Value>>,
    pub calls: AtomicUsize,
    pub failures: AtomicUsize,
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
