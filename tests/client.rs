use debugbundle::{
    Client, Config, Context, DeploySnapshot, ExceptionSnapshot, Level, RequestSnapshot,
};
use serde_json::json;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

mod helpers;
use helpers::{configured, emit_until, Recorder};

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
fn manual_aliases_and_deploy_metadata_preserve_protected_payloads() {
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    let context = Context::new(&json!({"apiKey":"SYNTHETIC_CONTEXT"}))
        .with_request_id("manual-alias-request");
    emit_until(
        &client,
        || {
            client.capture_error(
                &ExceptionSnapshot::new("ManualError", "password=SYNTHETIC_ERROR"),
                &context,
            )
        },
        || !recorder.events_without_pressure().is_empty(),
    );
    emit_until(
        &client,
        || client.capture_message("", Level::Warning, &context),
        || recorder.events_without_pressure().len() >= 2,
    );
    let deploy = DeploySnapshot {
        commit_sha: "synthetic-commit".into(),
        version: "1.2.3".into(),
        branch: "token=SYNTHETIC_BRANCH".into(),
        deployed_at: "2026-11-13T12:00:00Z".into(),
    };
    emit_until(
        &client,
        || client.capture_deploy(&deploy),
        || recorder.events_without_pressure().len() >= 3,
    );
    let events = recorder.events_without_pressure();
    assert_eq!(events.len(), 3);
    assert!(events.iter().all(debugbundle::wire::valid_event));
    assert!(!serde_json::to_string(&events)
        .unwrap()
        .contains("SYNTHETIC"));
    assert_eq!(events[0]["event_type"], "backend_exception");
    assert_eq!(events[0]["payload"]["name"], "ManualError");
    assert_eq!(events[0]["payload"]["message"], "password=[REDACTED]");
    assert_eq!(
        events[0]["correlation"]["request_id"],
        "manual-alias-request"
    );
    assert_eq!(events[1]["event_type"], "log_event");
    assert_eq!(events[1]["payload"]["level"], "warning");
    assert_eq!(events[1]["payload"]["message"], "Log message unavailable");
    assert_eq!(
        events[1]["correlation"]["request_id"],
        "manual-alias-request"
    );
    assert_eq!(events[2]["event_type"], "deploy_metadata");
    assert_eq!(events[2]["payload"]["commit_sha"], deploy.commit_sha);
    assert_eq!(events[2]["payload"]["version"], deploy.version);
    assert_eq!(events[2]["payload"]["branch"], "token=[REDACTED]");
    assert_eq!(events[2]["payload"]["deployed_at"], deploy.deployed_at);
    assert!(client.shutdown(Duration::from_secs(2)).drained);
}

#[test]
fn manual_request_snapshots_bound_targets_and_bodies_before_transport() {
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    for (index, (path, expected_path)) in [
        (format!("/{}", "x".repeat(16384)), "/[withheld]"),
        ("not a URL".into(), "/[unavailable]"),
        ("/checkout?token=SYNTHETIC_QUERY".into(), "/checkout"),
    ]
    .into_iter()
    .enumerate()
    {
        let request = RequestSnapshot {
            method: "METHOD_TOO_LONG_TO_RETAIN".into(),
            path,
            headers: json!({"authorization":"Bearer SYNTHETIC_HEADER"}),
            body: Some(json!({"value":"x".repeat(8193)})),
            status: 500,
            ..RequestSnapshot::default()
        };
        emit_until(
            &client,
            || client.capture_request(&request, &Context::default()),
            || recorder.events_without_pressure().len() > index,
        );
        let events = recorder.events_without_pressure();
        assert_eq!(events.len(), index + 1);
        let event = &events[index];
        assert!(debugbundle::wire::valid_event(event));
        assert_eq!(event["event_type"], "request_event");
        assert_eq!(event["payload"]["method"], "UNKNOWN");
        assert_eq!(event["payload"]["path"], expected_path);
        assert_eq!(event["payload"]["response_status"], 500);
        assert!(event["payload"]["body"].is_null());
        assert!(!event.to_string().contains("SYNTHETIC"));
        if index == 2 {
            assert_eq!(event["payload"]["query"]["token"], "[REDACTED]");
        } else {
            assert_eq!(event["payload"]["query"], json!({}));
        }
    }
    assert!(client.shutdown(Duration::from_secs(2)).drained);
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
