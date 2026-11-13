mod helpers;
use debugbundle::{
    delivery::{Transport, TransportFailure, TransportResponse},
    Client, Config, ConnectionStatus, Context, Level,
};
use helpers::{configured, Recorder};
use serde_json::json;
use std::{
    sync::{mpsc, Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

#[test]
fn process_client_capacity_is_bounded_and_released_after_drop() {
    let clients = (0..8)
        .map(|_| Client::new(configured(Arc::new(Recorder::default()))))
        .collect::<Vec<_>>();
    assert!(clients
        .iter()
        .all(|client| client.status().state == ConnectionStatus::Healthy));
    let rejected = std::panic::catch_unwind(|| {
        Client::new(Config {
            transport: Some(Arc::new(PanickingConfigurationDrop)),
            ..Config::new("synthetic", "service")
        })
    });
    assert!(
        rejected.is_ok(),
        "client-limit rejection unwound into the application"
    );
    assert_eq!(rejected.unwrap().status().reason, Some("client_limit"));
    drop(clients);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let client = Client::new(configured(Arc::new(Recorder::default())));
        if client.status().state == ConnectionStatus::Healthy {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}
struct BlockingDrop {
    gate: Arc<(Mutex<bool>, Condvar)>,
    entered: mpsc::Sender<()>,
}
impl Drop for BlockingDrop {
    fn drop(&mut self) {
        let _ = self.entered.send(());
        let mut released = self.gate.0.lock().unwrap();
        while !*released {
            let (result, timeout) = self
                .gate
                .1
                .wait_timeout(released, Duration::from_secs(2))
                .unwrap();
            released = result;
            if timeout.timed_out() {
                break;
            }
        }
    }
}
impl Transport for BlockingDrop {
    fn send(&self, _: &[u8], _: Duration) -> Result<TransportResponse, TransportFailure> {
        Ok(TransportResponse {
            status: 202,
            body: Some(json!({"accepted":1,"rejected":0,"errors":[]})),
            retry_after: None,
        })
    }
}
#[test]
fn application_configuration_destructors_do_not_stall_the_shared_sdk_runtime() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (tx, rx) = mpsc::channel();
    let client = Client::new(Config {
        transport: Some(Arc::new(BlockingDrop {
            gate: gate.clone(),
            entered: tx,
        })),
        ..Config::new("synthetic", "service")
    });
    let drop_start = Instant::now();
    drop(client);
    let drop_elapsed = drop_start.elapsed();
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let start = Instant::now();
    let recorder = Arc::new(Recorder::default());
    let independent = Client::new(configured(recorder.clone()));
    independent.capture_log(Level::Error, "independent", &Context::default());
    let outcome = independent.flush(Duration::from_millis(500));
    let elapsed = start.elapsed();
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    assert!(
        drop_elapsed < Duration::from_millis(500),
        "client drop ran an application destructor on its caller"
    );
    assert!(
        outcome.drained && elapsed < Duration::from_secs(1),
        "another client's destructor blocked SDK delivery"
    );
    assert_eq!(recorder.events.lock().unwrap().len(), 1);
}

struct PanickingConfigurationDrop;
impl Drop for PanickingConfigurationDrop {
    fn drop(&mut self) {
        panic!("synthetic configuration destructor");
    }
}
impl Transport for PanickingConfigurationDrop {
    fn send(&self, _: &[u8], _: Duration) -> Result<TransportResponse, TransportFailure> {
        unreachable!("rejected configuration must not send")
    }
}

#[test]
fn rejected_configuration_destructor_panics_cannot_escape_client_initialization() {
    for (enabled, service, expected) in [
        (false, "service", "disabled"),
        (true, "", "invalid_configuration"),
    ] {
        let result = std::panic::catch_unwind(|| {
            let captured = PanickingConfigurationDrop;
            Client::new(Config {
                enabled,
                before_send: Some(Arc::new(move |event| {
                    let _ = &captured;
                    Some(event)
                })),
                transport: Some(Arc::new(PanickingConfigurationDrop)),
                ..Config::new("synthetic", service)
            })
        });
        assert!(
            result.is_ok(),
            "configuration rejection unwound into the application"
        );
        assert_eq!(result.unwrap().status().reason, Some(expected));
    }
}

#[test]
fn rejected_configuration_cleanup_restores_application_capture_state() {
    let _ = Client::new(Config {
        enabled: false,
        ..Config::default()
    });
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    client.capture_log(
        Level::Error,
        "after rejected configuration",
        &Context::default(),
    );
    assert_eq!(client.flush(Duration::from_secs(1)).accepted_events, 1);
    assert_eq!(recorder.events.lock().unwrap().len(), 1);
}
