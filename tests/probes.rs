mod helpers;
use debugbundle::{Client, Config, Context, ExceptionSnapshot, ProbeOptions};
use helpers::{configured, Recorder};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

#[test]
fn always_on_probes_wrap_values_and_clear_only_with_an_admitted_error() {
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    client.probe(
        "cart.total",
        &json!(42),
        ProbeOptions::default(),
        &Context::default(),
    );
    client.probe(
        "cart.auth",
        &json!({"password":"SYNTHETIC_SECRET"}),
        ProbeOptions::default(),
        &Context::default(),
    );
    client.capture_exception(
        &ExceptionSnapshot::new("First", "failure"),
        &Context::default(),
    );
    client.capture_exception(
        &ExceptionSnapshot::new("Second", "failure"),
        &Context::default(),
    );
    assert!(client.flush(Duration::from_secs(2)).drained);
    let events = recorder.events.lock().unwrap();
    let items = events[0]["payload"]["probe_data"]["items"]
        .as_array()
        .unwrap();
    assert_eq!(items.len(), 2);
    assert!(items.iter().any(|i| i["data"] == json!({"value":42})));
    assert!(!serde_json::to_string(items)
        .unwrap()
        .contains("SYNTHETIC_SECRET"));
    assert!(events[1]["payload"].get("probe_data").is_none());
}

#[test]
fn lazy_callbacks_run_off_capture_once_and_heavy_callbacks_stay_dormant() {
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let producer = Arc::new(move || {
        observed.fetch_add(1, Ordering::SeqCst);
        json!({"answer":42})
    });
    client.probe_lazy(
        "heavy",
        &producer,
        ProbeOptions { heavy: true },
        &Context::default(),
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    client.probe_lazy(
        "normal",
        &producer,
        ProbeOptions::default(),
        &Context::default(),
    );
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    client.capture_exception(
        &ExceptionSnapshot::new("Error", "failure"),
        &Context::default(),
    );
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert_eq!(
        recorder.events.lock().unwrap()[0]["payload"]["probe_data"]["items"][0]["data"],
        json!({"answer":42})
    );
}

#[test]
fn disabled_or_full_probe_labels_invoke_no_producer() {
    let recorder = Arc::new(Recorder::default());
    let mut config = configured(recorder);
    config.max_probe_labels = 1;
    let client = Client::new(config);
    client.probe(
        "occupied",
        &Value::Null,
        ProbeOptions::default(),
        &Context::default(),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let producer = Arc::new(move || {
        observed.fetch_add(1, Ordering::SeqCst);
        Value::Null
    });
    client.probe_lazy(
        "new",
        &producer,
        ProbeOptions::default(),
        &Context::default(),
    );
    assert!(client.flush(Duration::from_secs(1)).drained);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let disabled = Client::new(Config {
        enabled: false,
        ..Config::default()
    });
    disabled.probe_lazy(
        "ignored",
        &producer,
        ProbeOptions::default(),
        &Context::default(),
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn probe_flush_option_and_stale_operation_ownership_are_respected() {
    let recorder = Arc::new(Recorder::default());
    let mut config = configured(recorder.clone());
    config.probe_flush_on_error = false;
    let client = Client::new(config);
    client.probe(
        "retained",
        &json!(true),
        ProbeOptions::default(),
        &Context::default(),
    );
    client.capture_exception(
        &ExceptionSnapshot::new("Error", "failure"),
        &Context::default(),
    );
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert!(recorder.events.lock().unwrap()[0]["payload"]
        .get("probe_data")
        .is_none());
    let operation = client.start_operation(debugbundle::OperationOptions::default());
    let stale = operation.context();
    drop(operation);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let producer = Arc::new(move || {
        observed.fetch_add(1, Ordering::SeqCst);
        json!(true)
    });
    client.probe_lazy("stale", &producer, ProbeOptions::default(), &stale);
    assert!(client.flush(Duration::from_secs(1)).drained);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn full_protocol_probe_array_is_not_mistaken_for_an_untrusted_application_container() {
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    for label in 0..50 {
        for _ in 0..10 {
            client.probe(
                &format!("label.{label}"),
                &json!(1),
                ProbeOptions::default(),
                &Context::default(),
            );
        }
    }
    client.capture_exception(
        &ExceptionSnapshot::new("Error", "failure"),
        &Context::default(),
    );
    assert!(client.flush(Duration::from_secs(3)).drained);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0]["payload"]["probe_data"]["items"]
            .as_array()
            .unwrap()
            .len(),
        500
    );
}
