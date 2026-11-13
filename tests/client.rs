use debugbundle::{Client, Config, Context, ExceptionSnapshot, Level};
use serde_json::json;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

mod helpers;
use helpers::{configured, Recorder};

#[test]
fn public_capture_is_protected_canonical_and_explicitly_flushed() {
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    client.capture_exception(
        &ExceptionSnapshot::new("CheckoutError", "password=SYNTHETIC_SECRET"),
        &Context::new(&json!({"apiKey":"SYNTHETIC_KEY"})).with_request_id("req-manual-42"),
    );
    client.capture_log(Level::Info, "filtered", &Context::default());
    client.capture_log(Level::Warning, "warning", &Context::default());
    assert!(client.flush(Duration::from_secs(2)).drained);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(debugbundle::wire::valid_event));
    assert!(!serde_json::to_string(&*events)
        .unwrap()
        .contains("SYNTHETIC"));
    assert!(events[0]["payload"].get("exception_evidence").is_none());
    assert_eq!(events[0]["correlation"]["request_id"], "req-manual-42");
    assert!(client.last_event_at().is_some());
    assert_eq!(client.status().buffered_events, 0);
}

#[test]
fn hooks_are_deferred_revalidated_once_and_may_replace_identity() {
    let recorder = Arc::new(Recorder::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let replacement = uuid::Uuid::new_v4().to_string();
    let expected = replacement.clone();
    let mut config = configured(recorder.clone());
    config.before_send = Some(Arc::new(move |mut event| {
        observed.fetch_add(1, Ordering::SeqCst);
        event["event_id"] = json!(replacement);
        event["payload"]["message"] = json!("token=SYNTHETIC_REINTRODUCED");
        Some(event)
    }));
    let client = Client::new(config);
    client.capture_log(Level::Info, "filtered", &Context::default());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    client.capture_exception(
        &ExceptionSnapshot::new("Error", "failure"),
        &Context::default(),
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events[0]["event_id"], expected);
    assert_eq!(events[0]["payload"]["message"], "token=[REDACTED]");
}

#[test]
fn invalid_acknowledgements_keep_the_same_finalized_event_without_hook_repetition() {
    let recorder = Arc::new(Recorder::default());
    recorder.failures.store(1, Ordering::SeqCst);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut config = configured(recorder.clone());
    config.before_send = Some(Arc::new(move |event| {
        observed.fetch_add(1, Ordering::SeqCst);
        Some(event)
    }));
    let client = Client::new(config);
    client.capture_log(Level::Error, "failure", &Context::default());
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(recorder.events.lock().unwrap().len(), 1);
}

#[test]
fn queue_pressure_preserves_primary_errors_and_full_high_priority_capture_is_fast() {
    let recorder = Arc::new(Recorder::default());
    let mut config = configured(recorder.clone());
    config.max_buffered_events = 2;
    let client = Client::new(config);
    client.capture_log(Level::Warning, "first", &Context::default());
    client.capture_log(Level::Warning, "second", &Context::default());
    client.capture_exception(
        &ExceptionSnapshot::new("Primary", "failure"),
        &Context::default(),
    );
    assert_eq!(client.status().buffered_events, 2);
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert!(recorder
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|e| e["payload"]["name"] == "Primary"));
    client.capture_log(Level::Error, "one", &Context::default());
    client.capture_log(Level::Error, "two", &Context::default());
    let start = Instant::now();
    for _ in 0..1000 {
        client.capture_log(Level::Error, "dropped", &Context::default());
    }
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(client.status().buffered_events <= 2);
    assert!(client.status().dropped_events >= 1000);
}

#[test]
fn missing_configuration_and_zero_sampling_do_not_report_delivery() {
    let client = Client::new(Config::default());
    client.capture_log(Level::Error, "ignored", &Context::default());
    assert!(client.last_event_at().is_none());
    let recorder = Arc::new(Recorder::default());
    let mut config = configured(recorder.clone());
    config.sample_rate = 0.0;
    let client = Client::new(config);
    client.capture_log(Level::Error, "sampled out", &Context::default());
    assert!(client.flush(Duration::from_secs(1)).drained);
    assert!(recorder.events.lock().unwrap().is_empty());
    assert!(client.last_event_at().is_none());
}
