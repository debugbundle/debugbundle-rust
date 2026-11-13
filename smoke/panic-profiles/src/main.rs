use debugbundle::{
    delivery::{Transport, TransportFailure, TransportResponse},
    Client, Config,
};
use serde_json::json;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

struct Sink(Arc<AtomicUsize>);
impl Transport for Sink {
    fn send(&self, body: &[u8], _: Duration) -> Result<TransportResponse, TransportFailure> {
        let value: serde_json::Value =
            serde_json::from_slice(body).map_err(|_| TransportFailure)?;
        let events = value["events"].as_array().ok_or(TransportFailure)?;
        if !events.iter().all(|event| {
            event["event_type"] == "backend_exception"
                || (event["event_type"] == "error_suppressed"
                    && event["payload"]["reason"] == "queue_pressure")
        }) {
            return Err(TransportFailure);
        }
        self.0.fetch_add(
            events
                .iter()
                .filter(|event| event["event_type"] == "backend_exception")
                .count(),
            Ordering::SeqCst,
        );
        Ok(TransportResponse {
            status: 202,
            body: Some(json!({"accepted":events.len(),"rejected":0,"errors":[]})),
            retry_after: None,
        })
    }
}

fn main() {
    let previous = Arc::new(AtomicUsize::new(0));
    let observed = previous.clone();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        observed.fetch_add(1, Ordering::SeqCst);
        eprintln!("previous-hook");
        // Preserve assertion diagnostics as well as the synthetic panic receipt.
        default_hook(info);
    }));
    let hooks = Arc::new(AtomicUsize::new(0));
    let observed = hooks.clone();
    let received = Arc::new(AtomicUsize::new(0));
    let client = Client::new(Config {
        transport: Some(Arc::new(Sink(received.clone()))),
        before_send: Some(Arc::new(move |event| {
            observed.fetch_add(1, Ordering::SeqCst);
            Some(event)
        })),
        ..Config::new("synthetic", "panic-smoke")
    });
    let registration = client.install_panic_observer();
    assert!(registration.is_active());
    if std::env::args().nth(1).as_deref() == Some("abort") {
        panic!("synthetic abort profile");
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut attempts = 0;
    loop {
        assert!(std::panic::catch_unwind(|| panic!("synthetic unwind profile")).is_err());
        attempts += 1;
        assert!(client.flush(Duration::from_secs(2)).drained);
        assert_eq!(previous.load(Ordering::SeqCst), attempts);
        assert_eq!(hooks.load(Ordering::SeqCst), 0);
        let accepted = received.load(Ordering::SeqCst);
        assert!(
            accepted <= 1,
            "one panic attempt must never duplicate its event"
        );
        if accepted == 1 {
            break;
        }
        // A panic observer is nonblocking best effort even during sender-state
        // contention. Prior application hooks must still run for every attempt.
        assert!(
            Instant::now() < deadline,
            "panic observer never admitted an event"
        );
        std::thread::yield_now();
    }
    println!("unwind-survived");
}
