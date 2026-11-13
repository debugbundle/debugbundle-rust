use super::*;
use std::{
    sync::{atomic::AtomicUsize, mpsc, Condvar, Mutex},
    time::{Duration, Instant},
};
struct Sink;
impl crate::delivery::Transport for Sink {
    fn send(
        &self,
        body: &[u8],
        _: Duration,
    ) -> Result<crate::delivery::TransportResponse, crate::delivery::TransportFailure> {
        let value: Value = serde_json::from_slice(body).unwrap();
        Ok(crate::delivery::TransportResponse {
            status: 202,
            body: Some(
                json!({"accepted":value["events"].as_array().unwrap().len(),"rejected":0,"errors":[]}),
            ),
            retry_after: None,
        })
    }
}

#[test]
fn full_event_ownership_skips_lazy_producers_even_when_the_callback_lane_is_idle() {
    let client = Client::new(crate::Config {
        transport: Some(Arc::new(Sink)),
        batch_size: 100,
        max_buffered_events: 1,
        flush_interval: Duration::from_secs(300),
        ..crate::Config::new("synthetic", "service")
    });
    client.capture_exception(
        &crate::ExceptionSnapshot::new("Held", "queued"),
        &Context::default(),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let producer = Arc::new(move || {
        observed.fetch_add(1, Ordering::SeqCst);
        json!(true)
    });
    let inner = client.inner.as_ref().unwrap();
    assert_eq!(inner.state.lock().unwrap().owned, 1);
    assert!(inner.callbacks.available());
    client.probe_lazy(
        "pressure",
        &producer,
        ProbeOptions::default(),
        &Context::default(),
    );
    let scheduled = inner.state.lock().unwrap().pending_probe.is_some();
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert!(
        !scheduled,
        "full queues must reject before scheduling a lazy producer"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    client.probe_lazy(
        "available",
        &producer,
        ProbeOptions::default(),
        &Context::default(),
    );
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
struct Dropped {
    gate: Arc<(Mutex<bool>, Condvar)>,
    entered: mpsc::Sender<()>,
}
impl Drop for Dropped {
    fn drop(&mut self) {
        let _ = self.entered.send(());
        let released = self.gate.0.lock().unwrap();
        let _ = self
            .gate
            .1
            .wait_timeout_while(released, Duration::from_secs(2), |released| !*released)
            .unwrap();
    }
}
#[test]
fn unavailable_producer_disposal_does_not_run_on_the_sdk_io_thread() {
    let config = || crate::Config {
        transport: Some(Arc::new(Sink)),
        ..crate::Config::new("synthetic", "service")
    };
    let source = Client::new(config());
    let independent = Client::new(config());
    let park = Arc::new((Mutex::new(false), Condvar::new()));
    let parked = park.clone();
    let (tx, rx) = mpsc::channel();
    crate::runtime::spawn(async move {
        tx.send(()).unwrap();
        let ready = parked.0.lock().unwrap();
        let _ = parked
            .1
            .wait_timeout_while(ready, Duration::from_secs(2), |ready| !*ready)
            .unwrap();
    });
    rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (tx, entered) = mpsc::channel();
    let guard = Dropped {
        gate: gate.clone(),
        entered: tx,
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let producer = Arc::new(move || {
        let _keep = &guard;
        observed.fetch_add(1, Ordering::SeqCst);
        json!(true)
    });
    source.probe_lazy(
        "pending",
        &producer,
        ProbeOptions::default(),
        &Context::default(),
    );
    drop(producer);
    struct Opaque;
    let test_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result: Result<(), crate::runtime::CallbackFailure> = test_runtime.block_on(
        source
            .inner
            .as_ref()
            .unwrap()
            .callbacks
            .call(|| std::panic::panic_any(Opaque)),
    );
    assert!(matches!(
        result,
        Err(crate::runtime::CallbackFailure::Panicked)
    ));
    *park.0.lock().unwrap() = true;
    park.1.notify_all();
    entered.recv_timeout(Duration::from_secs(1)).unwrap();
    let start = Instant::now();
    independent.capture_log(crate::Level::Error, "independent", &Context::default());
    let outcome = independent.flush(Duration::from_millis(500));
    let elapsed = start.elapsed();
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    assert!(
        outcome.drained && elapsed < Duration::from_secs(1),
        "unavailable callback disposal blocked the SDK runtime"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(source.flush(Duration::from_secs(2)).drained);
}
