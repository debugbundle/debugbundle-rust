use debugbundle::{
    Client, Config, Context, DeploySnapshot, EvidenceScope, ExceptionSnapshot, FrameClassification,
    FrameSnapshot, Level, OperationKind, OperationOptions, RecoveryObservation, RecoveryState,
    RequestSnapshot, TraceOrigin,
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

#[test]
fn redact_exact_fields_masks_exact_keys_while_preserving_compound_siblings() {
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()).with_redact_exact_fields(["tag"]));
    client.set_context(&Context::new(&json!({
        "payout": {"tag": "SYNTHETIC_CTX_TAG", "TAG": "SYNTHETIC_CTX_UPPER", "tag_name": "script"},
        "tag_name": "script",
        "tagName": "link",
        "tag-name": "img"
    })));
    emit_until(
        &client,
        || {
            client.capture_log(
                Level::Error,
                "payout failed tag=SYNTHETIC_LOG_TAG tag_name=script tag-name=img",
                &Context::new(&json!({
                    "tag": "SYNTHETIC_LOCAL_TAG",
                    "tag_name": "script",
                    "url": "https://example.test/api?tag=SYNTHETIC_QUERY&tag_name=script"
                })),
            );
        },
        || !recorder.events_without_pressure().is_empty(),
    );
    let request = RequestSnapshot {
        method: "POST".into(),
        path: "/payout?tag=SYNTHETIC_REQ_TAG&tag_name=script".into(),
        headers: json!({"tag": "SYNTHETIC_HDR_TAG", "tag-name": "img"}),
        body: Some(json!({"tag": "SYNTHETIC_BODY_TAG", "tag_name": "script"})),
        status: 500,
        ..RequestSnapshot::default()
    };
    emit_until(
        &client,
        || client.capture_request(&request, &Context::default()),
        || recorder.events_without_pressure().len() >= 2,
    );
    let events = recorder.events_without_pressure();
    assert_eq!(events.len(), 2);
    assert!(!serde_json::to_string(&events)
        .unwrap()
        .contains("SYNTHETIC_"));

    let log = &events[0];
    assert_eq!(
        log["payload"]["message"],
        "payout failed tag=[REDACTED] tag_name=script tag-name=img"
    );
    assert_eq!(log["context"]["tag"], "[REDACTED]");
    assert_eq!(log["context"]["tag_name"], "script");
    assert_eq!(log["context"]["tagName"], "link");
    assert_eq!(log["context"]["tag-name"], "img");
    assert_eq!(log["context"]["payout"]["tag"], "[REDACTED]");
    assert_eq!(log["context"]["payout"]["TAG"], "[REDACTED]");
    assert_eq!(log["context"]["payout"]["tag_name"], "script");
    assert_eq!(
        log["context"]["url"],
        "https://example.test/api?tag=%5BREDACTED%5D&tag_name=script"
    );

    let req = &events[1];
    assert_eq!(req["payload"]["query"]["tag"], "[REDACTED]");
    assert_eq!(req["payload"]["query"]["tag_name"], "script");
    assert_eq!(req["payload"]["headers"]["tag"], "[REDACTED]");
    assert_eq!(req["payload"]["headers"]["tag-name"], "img");
    assert_eq!(req["payload"]["body"]["tag"], "[REDACTED]");
    assert_eq!(req["payload"]["body"]["tag_name"], "script");
    assert!(client.shutdown(Duration::from_secs(2)).drained);
}

#[test]
fn broad_redact_fields_preserves_validated_protocol_evidence_and_redacts_application_fields() {
    let recorder = Arc::new(Recorder::default());
    let failure_ref = "11111111-1111-4111-8111-111111111111";
    let attempt_id = "22222222-2222-4222-8222-222222222222";
    let mut config =
        configured(recorder.clone()).with_redact_fields(["tag", "reference", "reason", "file"]);
    config.capture_recovery_observations = true;
    config.evidence_capabilities = Some(json!({
        "version": 1,
        "features": ["exception_evidence_v1", "recovery_v1", "evidence_provenance_v1"]
    }));
    config.before_send = Some(Arc::new(|mut event| {
        if event["event_type"] == "backend_exception" {
            event["context"]["payout"]["tag"] = json!("SYNTHETIC_HOOK_TAG");
            event["payload"]["exception_evidence"]["nodes"][0]["message"] =
                json!("reintroduced reason=SYNTHETIC_HOOK_REASON");
            event["payload"]["execution"]["name"] = json!("payout.run tag=SYNTHETIC_HOOK_EXEC");
        }
        Some(event)
    }));

    let client = Client::new(config);
    let op = client.start_operation(OperationOptions {
        kind: OperationKind::Job,
        name: "payout.run".into(),
        limit: Some(Duration::from_secs(10)),
        context: Context::new(&json!({
            "payout": {
                "tag": "SYNTHETIC_TAG",
                "reference": "SYNTHETIC_REF",
                "reason": "SYNTHETIC_REASON",
                "file": "SYNTHETIC_FILE"
            }
        })),
        ..OperationOptions::default()
    });
    let op_id = op.handle().operation_id().unwrap().to_owned();
    let scope = EvidenceScope::Operation {
        operation_id: op_id.clone(),
    };
    let context = op.context().with_evidence_reference(failure_ref, &scope);
    let snapshot = ExceptionSnapshot::new("PayoutError", "failed reason=SYNTHETIC_MSG_REASON")
        .with_frames(
            &[
                FrameSnapshot {
                    function: Some("execute_payout".into()),
                    file: Some("/app/src/payout.rs".into()),
                    line: Some(42),
                    column: Some(7),
                    classification: FrameClassification::Application,
                },
                FrameSnapshot {
                    function: Some("fetch_remote".into()),
                    file: Some(
                        "https://cdn.example.test/app.js?token=SYNTHETIC_URL_TOKEN&file=SYNTHETIC_URL_FILE"
                            .into(),
                    ),
                    line: Some(11),
                    column: Some(3),
                    classification: FrameClassification::Framework,
                },
            ],
            TraceOrigin::Capture,
        );

    emit_until(
        &client,
        || client.capture_exception(&snapshot, &context),
        || {
            recorder
                .events_without_pressure()
                .iter()
                .any(|e| e["event_type"] == "backend_exception")
        },
    );
    let recovery = RecoveryObservation {
        failure_ref: failure_ref.into(),
        attempt_id: attempt_id.into(),
        state: RecoveryState::RecoveryObserved,
        operation: Some("retry tag=SYNTHETIC_OP_TAG".into()),
        status_code: Some(200),
        duration: Some(Duration::from_millis(45)),
        ..Default::default()
    };
    emit_until(
        &client,
        || client.capture_recovery(&recovery, &context),
        || {
            recorder
                .events_without_pressure()
                .iter()
                .any(|e| e["event_type"] == "diagnostic_context")
        },
    );

    let events = recorder.events_without_pressure();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(debugbundle::wire::valid_event));
    assert!(!serde_json::to_string(&events)
        .unwrap()
        .contains("SYNTHETIC_"));

    let exc = events
        .iter()
        .find(|e| e["event_type"] == "backend_exception")
        .unwrap();
    assert_eq!(
        exc["context"]["evidence_reference"],
        json!({
            "version": 1,
            "failure_ref": failure_ref,
            "scope": {"kind": "operation", "operation_id": op_id}
        })
    );
    assert_eq!(
        exc["context"]["payout"],
        json!({
            "tag": "[REDACTED]",
            "reference": "[REDACTED]",
            "reason": "[REDACTED]",
            "file": "[REDACTED]"
        })
    );
    assert!(exc["payload"]["exception_evidence"]["unavailable_reason"].is_null());
    assert_eq!(
        exc["payload"]["exception_evidence"]["nodes"][0]["message"],
        "reintroduced reason=[REDACTED]"
    );
    assert_eq!(
        exc["payload"]["exception_evidence"]["nodes"][0]["frames"][0]["file"],
        "/app/src/payout.rs"
    );
    assert_eq!(
        exc["payload"]["exception_evidence"]["nodes"][0]["frames"][1]["file"],
        "https://cdn.example.test/app.js?token=%5BREDACTED%5D&file=%5BREDACTED%5D"
    );
    assert_eq!(exc["payload"]["execution"]["kind"], "job");
    assert_eq!(
        exc["payload"]["execution"]["name"],
        "payout.run tag=[REDACTED]"
    );

    let diag = events
        .iter()
        .find(|e| e["event_type"] == "diagnostic_context")
        .unwrap();
    assert_eq!(diag["payload"]["failure_ref"], failure_ref);
    assert_eq!(diag["payload"]["attempt_id"], attempt_id);
    assert_eq!(diag["payload"]["scope"]["operation_id"], op_id);
    assert_eq!(diag["payload"]["operation"], "retry tag=[REDACTED]");
    assert_eq!(diag["context"]["payout"]["reference"], "[REDACTED]");
    assert!(client.shutdown(Duration::from_secs(2)).drained);
}
