mod helpers;
use debugbundle::{
    delivery::{Transport, TransportFailure, TransportResponse},
    Client, Config, Context, ExceptionSnapshot, Level,
};
use helpers::{configured, Recorder};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};

#[test]
fn owner_replaced_mathematical_integer_status_keeps_failure_policy() {
    let recorder = Arc::new(Recorder::default());
    let mut config = configured(recorder.clone());
    config.before_send = Some(Arc::new(|mut event| {
        event["payload"]["response_status"] = json!(500.0);
        Some(event)
    }));
    let client = Client::new(config);
    client.capture_request(
        &debugbundle::RequestSnapshot {
            method: "GET".into(),
            path: "/synthetic".into(),
            status: 500,
            ..debugbundle::RequestSnapshot::default()
        },
        &Context::default(),
    );
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert_eq!(recorder.events.lock().unwrap().len(), 1);
}

#[test]
fn blocked_hooks_do_not_block_capture_or_finite_flush_and_keep_ownership_charged() {
    let recorder = Arc::new(Recorder::default());
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let held = gate.clone();
    let (tx, rx) = mpsc::sync_channel(1);
    let mut config = configured(recorder.clone());
    config.before_send = Some(Arc::new(move |event| {
        let _ = tx.send(());
        let (lock, changed) = &*held;
        let mut ready = lock.lock().unwrap();
        while !*ready {
            ready = changed.wait(ready).unwrap();
        }
        Some(event)
    }));
    let client = Client::new(config);
    client.capture_log(Level::Error, "held", &Context::default());
    let flushing = client.clone();
    let background = std::thread::spawn(move || flushing.flush(Duration::from_secs(2)));
    rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let start = Instant::now();
    let outcome = client.flush(Duration::from_millis(20));
    assert!(outcome.timed_out);
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(client.status().buffered_events, 1);
    assert!(client.status().buffered_bytes > 0);
    let start = Instant::now();
    client.capture_log(Level::Error, "second", &Context::default());
    assert!(start.elapsed() < Duration::from_millis(250));
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    assert!(background.join().unwrap().drained);
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert_eq!(recorder.events.lock().unwrap().len(), 2);
}

#[test]
fn valid_replacements_over_budget_are_withheld_and_never_restore_original_content() {
    let recorder = Arc::new(Recorder::default());
    let mut config = configured(recorder.clone());
    config.before_send = Some(Arc::new(|mut event| {
        event["context"] = json!({"large":vec!["x".repeat(16384);17]});
        Some(event)
    }));
    let client = Client::new(config);
    client.capture_exception(
        &ExceptionSnapshot::new(
            "MustNotRestore",
            "original private content removed by owner hook",
        ),
        &Context::default(),
    );
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert!(recorder.events.lock().unwrap().is_empty());
    assert!(client.last_event_at().is_none());
}

#[test]
fn invalid_and_panicking_hooks_use_the_safe_original_and_drop_is_explicit() {
    for mode in 0..3 {
        let recorder = Arc::new(Recorder::default());
        let mut config = configured(recorder.clone());
        config.before_send = Some(Arc::new(move |_| match mode {
            0 => Some(json!(false)),
            1 => panic!("synthetic callback failure"),
            _ => None,
        }));
        let client = Client::new(config);
        client.capture_log(
            Level::Error,
            "password=SYNTHETIC_SECRET",
            &Context::default(),
        );
        assert!(client.flush(Duration::from_secs(2)).drained);
        let events = recorder.events.lock().unwrap();
        if mode == 2 {
            assert!(events.is_empty());
        } else {
            assert_eq!(events[0]["payload"]["message"], "password=[REDACTED]");
        }
    }
}

struct Partial {
    batches: Mutex<Vec<Vec<String>>>,
}
impl Transport for Partial {
    fn send(&self, body: &[u8], _: Duration) -> Result<TransportResponse, TransportFailure> {
        let value: Value = serde_json::from_slice(body).unwrap();
        let ids = value["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["event_id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        let mut batches = self.batches.lock().unwrap();
        let first = batches.is_empty();
        let count = ids.len();
        batches.push(ids);
        Ok(TransportResponse {
            status: 202,
            body: Some(if first {
                json!({"accepted":1,"rejected":2,"errors":[{"index":1,"reason":"monthly_quota_exceeded"},{"index":2,"reason":"capture_policy_rejected"}]})
            } else {
                json!({"accepted":count,"rejected":0,"errors":[]})
            }),
            retry_after: Some(Duration::from_millis(1)),
        })
    }
}
#[test]
fn partial_acknowledgement_retries_only_the_retryable_index() {
    let transport = Arc::new(Partial {
        batches: Mutex::new(Vec::new()),
    });
    let config = Config {
        transport: Some(transport.clone()),
        batch_size: 100,
        ..Config::new("synthetic", "checkout")
    };
    let client = Client::new(config);
    for n in 0..3 {
        client.capture_log(Level::Error, &format!("failure {n}"), &Context::default());
    }
    assert!(client.flush(Duration::from_secs(2)).drained);
    let batches = transport.batches.lock().unwrap();
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].len(), 3);
    assert_eq!(batches[1], vec![batches[0][1].clone()]);
}

struct SizedRecorder {
    sizes: Mutex<Vec<usize>>,
    events: Mutex<Vec<String>>,
    hooks: AtomicUsize,
}
impl Transport for SizedRecorder {
    fn send(&self, body: &[u8], _: Duration) -> Result<TransportResponse, TransportFailure> {
        self.sizes.lock().unwrap().push(body.len());
        let value: Value = serde_json::from_slice(body).unwrap();
        let events = value["events"].as_array().unwrap();
        self.events.lock().unwrap().extend(
            events
                .iter()
                .map(|e| e["event_id"].as_str().unwrap().into()),
        );
        Ok(TransportResponse {
            status: 202,
            body: Some(json!({"accepted":events.len(),"rejected":0,"errors":[]})),
            retry_after: None,
        })
    }
}
#[test]
fn final_byte_chunks_preserve_all_identities_and_do_not_repeat_hooks() {
    let transport = Arc::new(SizedRecorder {
        sizes: Mutex::new(Vec::new()),
        events: Mutex::new(Vec::new()),
        hooks: AtomicUsize::new(0),
    });
    let observed = transport.clone();
    let config = Config {
        transport: Some(transport.clone()),
        batch_size: 100,
        before_send: Some(Arc::new(move |event| {
            observed.hooks.fetch_add(1, Ordering::Relaxed);
            Some(event)
        })),
        ..Config::new("synthetic", "checkout")
    };
    let client = Client::new(config);
    for n in 0..30 {
        client.capture_log(
            Level::Error,
            &format!("{n}: {}", "é".repeat(5000)),
            &Context::default(),
        );
    }
    assert!(client.flush(Duration::from_secs(10)).drained);
    let sizes = transport.sizes.lock().unwrap();
    assert!(sizes.len() >= 2);
    assert!(sizes.iter().all(|n| *n <= 262144));
    let events = transport.events.lock().unwrap();
    assert_eq!(events.len(), 30);
    assert_eq!(
        events
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        30
    );
    assert_eq!(transport.hooks.load(Ordering::Relaxed), 30);
}

#[test]
fn queue_pressure_has_one_bounded_operational_summary_when_capacity_returns() {
    let recorder = Arc::new(Recorder::default());
    let mut config = configured(recorder.clone());
    config.max_buffered_events = 1;
    let client = Client::new(config);
    client.capture_log(Level::Error, "primary", &Context::default());
    for _ in 0..100 {
        client.capture_log(Level::Error, "rejected", &Context::default());
    }
    assert!(client.flush(Duration::from_secs(2)).drained);
    let events = recorder.events.lock().unwrap();
    let summaries: Vec<_> = events
        .iter()
        .filter(|e| {
            e["event_type"] == "error_suppressed" && e["payload"]["reason"] == "queue_pressure"
        })
        .collect();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0]["payload"]["suppressed_count"], 100);
}

#[test]
fn optional_evidence_cannot_displace_an_otherwise_admissible_primary() {
    let recorder = Arc::new(Recorder::default());
    let config = configured(recorder.clone());
    let client = Client::new(config);
    let context = Context::new(&json!({"technical":vec!["x".repeat(15000);16]}));
    let snapshot = ExceptionSnapshot::new("Primary", &"failure".repeat(285)).with_stack(
        &"x".repeat(16000),
        debugbundle::TraceOrigin::RuntimeReported,
    );
    client.capture_exception(&snapshot, &context);
    assert!(client.flush(Duration::from_secs(3)).drained);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["payload"]["name"], "Primary");
}

struct OpaquePanic(Arc<AtomicUsize>);
impl Drop for OpaquePanic {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
        panic!("synthetic payload destructor failure");
    }
}
#[test]
fn opaque_callback_panics_are_discarded_without_bypassing_future_hooks() {
    let recorder = Arc::new(Recorder::default());
    let dropped = Arc::new(AtomicUsize::new(0));
    let observed = dropped.clone();
    let mut config = configured(recorder.clone());
    config.before_send = Some(Arc::new(move |_| {
        std::panic::panic_any(OpaquePanic(observed.clone()))
    }));
    let client = Client::new(config);
    client.capture_log(Level::Error, "first", &Context::default());
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    client.capture_log(
        Level::Error,
        "must not bypass owner hook",
        &Context::default(),
    );
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert!(recorder.events.lock().unwrap().is_empty());
    assert!(client.last_event_at().is_none());
}

#[test]
fn callbacks_cannot_recursively_capture_or_wait_for_their_own_flush() {
    let recorder = Arc::new(Recorder::default());
    let holder = Arc::new(Mutex::new(None::<Client>));
    let nested = holder.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut config = configured(recorder.clone());
    config.before_send = Some(Arc::new(move |event| {
        observed.fetch_add(1, Ordering::SeqCst);
        if let Some(client) = nested.lock().unwrap().as_ref() {
            client.capture_log(Level::Error, "recursive", &Context::default());
            let start = Instant::now();
            assert!(client.flush(Duration::from_secs(20)).timed_out);
            assert!(start.elapsed() < Duration::from_millis(250));
        }
        Some(event)
    }));
    let client = Client::new(config);
    *holder.lock().unwrap() = Some(client.clone());
    client.capture_log(Level::Error, "original", &Context::default());
    assert!(client.flush(Duration::from_secs(2)).drained);
    holder.lock().unwrap().take();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(recorder.events.lock().unwrap().len(), 1);
}

#[test]
fn oversized_optional_probe_fields_preserve_the_valid_owner_replacement() {
    let recorder = Arc::new(Recorder::default());
    let mut config = configured(recorder.clone());
    config.before_send = Some(Arc::new(|mut event| {
        event["payload"]["message"] = json!("owner replacement");
        let data = (0..257)
            .map(|index| (format!("technical_{index}"), json!(true)))
            .collect::<serde_json::Map<_, _>>();
        event["payload"]["probe_data"] = json!({"version":1,"items":[{"label":"technical","data":data,"timestamp":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"activation_id":null}]});
        Some(event)
    }));
    let client = Client::new(config);
    client.capture_exception(
        &ExceptionSnapshot::new("Primary", "original"),
        &Context::default(),
    );
    assert!(client.flush(Duration::from_secs(2)).drained);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["payload"]["message"], "owner replacement");
    assert!(events[0]["payload"].get("probe_data").is_none());
    assert_eq!(
        events[0]["context"]["probe_limits"]["reason"],
        "budget_exceeded"
    );
}

#[test]
fn optional_evidence_yields_to_actual_escaped_primary_bytes() {
    let context = Context::new(&json!({"technical":"\u{0001}".repeat(500)}));
    let snapshot = ExceptionSnapshot::new("Primary", "owner protected primary").with_stack(
        &"frame\n".repeat(300),
        debugbundle::TraceOrigin::RuntimeReported,
    );
    let baseline = Arc::new(Recorder::default());
    let mut config = configured(baseline.clone());
    config.capture_exception_evidence = false;
    let client = Client::new(config);
    client.capture_exception(&snapshot, &context);
    assert!(client.flush(Duration::from_secs(2)).drained);
    let primary = serde_json::to_vec(&baseline.events.lock().unwrap()[0])
        .unwrap()
        .len();
    drop(client);
    let recorder = Arc::new(Recorder::default());
    let mut config = configured(recorder.clone());
    config.max_buffered_bytes = primary + 256;
    config.before_send = Some(Arc::new(|mut event| {
        event["payload"]["message"] = json!("owner protected primary");
        Some(event)
    }));
    let client = Client::new(config);
    client.capture_exception(&snapshot, &context);
    assert!(client.flush(Duration::from_secs(2)).drained);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["payload"]["message"], "owner protected primary");
    assert!(serde_json::to_vec(&events[0]).unwrap().len() <= primary + 256);
}
