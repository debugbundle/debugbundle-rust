use debugbundle::{
    delivery::{Transport, TransportFailure, TransportResponse},
    Client, Config, ExceptionSnapshot,
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    time::Duration,
};

pub const SAMPLES: usize = 512;
pub struct Gate {
    released: Mutex<bool>,
    changed: Condvar,
    pub expired: AtomicBool,
    pub calls: AtomicUsize,
    entered: mpsc::Sender<()>,
}
impl Gate {
    pub fn new() -> (Arc<Self>, mpsc::Receiver<()>) {
        let (entered, receiver) = mpsc::channel();
        (
            Arc::new(Self {
                released: Mutex::new(false),
                changed: Condvar::new(),
                expired: AtomicBool::new(false),
                calls: AtomicUsize::new(0),
                entered,
            }),
            receiver,
        )
    }
    pub fn wait(&self) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let _ = self.entered.send(());
        let released = self.released.lock().unwrap();
        let (_released, result) = self
            .changed
            .wait_timeout_while(released, Duration::from_secs(10), |released| !*released)
            .unwrap();
        if result.timed_out() {
            self.expired.store(true, Ordering::Release);
        }
    }
    pub fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}
pub struct Ack {
    pub gate: Option<Arc<Gate>>,
    pub accepted: AtomicUsize,
}
impl Transport for Ack {
    fn send(&self, body: &[u8], _: Duration) -> Result<TransportResponse, TransportFailure> {
        if let Some(gate) = &self.gate {
            gate.wait();
        }
        let body: Value = serde_json::from_slice(body).map_err(|_| TransportFailure)?;
        let events = body["events"].as_array().ok_or(TransportFailure)?;
        self.accepted.fetch_add(events.len(), Ordering::Relaxed);
        Ok(TransportResponse {
            status: 202,
            body: Some(json!({"accepted":events.len(),"rejected":0,"errors":[]})),
            retry_after: None,
        })
    }
}
pub fn config(mode: &str, sink: Arc<Ack>) -> Config {
    Config {
        enabled: mode != "disabled",
        transport: Some(sink),
        batch_size: 100,
        flush_interval: Duration::from_secs(300),
        capture_exception_evidence: mode == "evidence",
        evidence_capabilities: Some(json!({"version":1,"features":debugbundle::wire::FEATURES})),
        ..Config::new("synthetic", "resource-benchmark")
    }
}
pub fn snapshots(count: usize) -> Vec<ExceptionSnapshot> {
    (0..count)
        .map(|index| {
            let name = format!(
                "Synthetic{}{}{}",
                (b'a' + ((index / 676) % 26) as u8) as char,
                (b'a' + ((index / 26) % 26) as u8) as char,
                (b'a' + (index % 26) as u8) as char
            );
            ExceptionSnapshot::new(&name, "synthetic safe failure").with_cause(
                &ExceptionSnapshot::new("StorageFailure", "safe owned cause"),
            )
        })
        .collect()
}
pub fn drain(client: &Client) {
    assert!(client.flush(Duration::from_secs(2)).drained);
}
pub fn sink(gate: Option<Arc<Gate>>) -> Arc<Ack> {
    Arc::new(Ack {
        gate,
        accepted: AtomicUsize::new(0),
    })
}
