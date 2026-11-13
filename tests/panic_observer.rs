mod helpers;
use debugbundle::{Client, Context, ExceptionSnapshot, ProbeOptions};
use helpers::{configured, Recorder};
use serde_json::json;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
#[test]
fn panic_observation_is_qualified_in_an_isolated_process() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "panic_observer_child", "--nocapture"])
        .env("DEBUGBUNDLE_PANIC_TEST_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn panic_observer_child() {
    if std::env::var("DEBUGBUNDLE_PANIC_TEST_CHILD").as_deref() != Ok("1") {
        return;
    }
    let previous = Arc::new(AtomicUsize::new(0));
    let observed = previous.clone();
    std::panic::set_hook(Box::new(move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
    }));
    let recorder = Arc::new(Recorder::default());
    let hooks = Arc::new(AtomicUsize::new(0));
    let observed = hooks.clone();
    let mut config = configured(recorder.clone());
    config.evidence_capabilities = Some(json!({"version":1,"features":["exception_evidence_v1"]}));
    config.before_send = Some(Arc::new(move |event| {
        observed.fetch_add(1, Ordering::SeqCst);
        Some(event)
    }));
    let client = Client::new(config);
    let registration = client.install_panic_observer();
    assert!(registration.is_active());
    assert!(!client.install_panic_observer().is_active());
    let mut retired = Vec::new();
    for _ in 0..8 {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let temporary = loop {
            let candidate = Client::new(configured(Arc::new(Recorder::default())));
            if candidate.status().state == debugbundle::ConnectionStatus::Healthy {
                break candidate;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        };
        let guard = temporary.install_panic_observer();
        assert!(
            guard.is_active(),
            "stopped client retained observer capacity"
        );
        temporary.shutdown(Duration::ZERO);
        drop(temporary);
        retired.push(guard);
    }
    client.probe(
        "retained",
        &json!(true),
        ProbeOptions::default(),
        &Context::default(),
    );
    assert!(std::panic::catch_unwind(|| panic!("password=SYNTHETIC_SECRET")).is_err());
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert_eq!(hooks.load(Ordering::SeqCst), 0);
    assert_eq!(previous.load(Ordering::SeqCst), 1);
    {
        let events = recorder.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event["payload"]["handled"], false);
        assert_eq!(
            event["payload"]["exception_evidence"]["nodes"][0]["trace_origin"],
            "unavailable"
        );
        assert!(event["context"]["panic"]["location"]["line"]
            .as_u64()
            .is_some());
        assert!(event["payload"].get("probe_data").is_none());
        assert!(!serde_json::to_string(event)
            .unwrap()
            .contains("SYNTHETIC_SECRET"));
    }
    struct Opaque(Arc<AtomicUsize>);
    impl Drop for Opaque {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let value = drops.clone();
    let payload = std::panic::catch_unwind(move || std::panic::panic_any(Opaque(value)));
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert!(client.flush(Duration::from_secs(2)).drained);
    drop(payload);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(previous.load(Ordering::SeqCst), 2);
    client.capture_exception(
        &ExceptionSnapshot::new("Ordinary", "failure"),
        &Context::default(),
    );
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert_eq!(hooks.load(Ordering::SeqCst), 1);
    assert_eq!(recorder.events.lock().unwrap().len(), 3);
    let replacement = Arc::new(AtomicUsize::new(0));
    let observed = replacement.clone();
    std::panic::set_hook(Box::new(move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
    }));
    drop(registration);
    assert!(std::panic::catch_unwind(|| panic!("after detach")).is_err());
    assert_eq!(replacement.load(Ordering::SeqCst), 1);
    assert_eq!(previous.load(Ordering::SeqCst), 2);
    assert!(client.flush(Duration::from_secs(1)).drained);
    assert_eq!(recorder.events.lock().unwrap().len(), 3);
}
