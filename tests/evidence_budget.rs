mod helpers;
use debugbundle::{privacy::Privacy, wire, Client, Context, ExceptionSnapshot, TraceOrigin};
use helpers::{configured, emit_until, Recorder};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

#[test]
fn unvalidated_optional_hook_fields_are_withheld_at_the_ownership_limit() {
    for field in [
        "exception_evidence",
        "execution",
        "resource_observations",
        "probe_data",
    ] {
        let recorder = Arc::new(Recorder::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let mut config = configured(recorder.clone());
        config.before_send = Some(Arc::new(move |mut event| {
            observed.fetch_add(1, Ordering::SeqCst);
            event["payload"]["message"] = json!("owner replacement");
            event["payload"][field] = json!("x".repeat(wire::MAX_BODY_BYTES + 1));
            Some(event)
        }));
        let client = Client::new(config);
        emit_until(
            &client,
            || {
                client.capture_exception(
                    &ExceptionSnapshot::new("Primary", "original must stay removed"),
                    &Context::default(),
                )
            },
            || calls.load(Ordering::SeqCst) > 0,
        );
        assert!(client.flush(Duration::from_secs(2)).drained);
        assert!(
            recorder.events_without_pressure().is_empty(),
            "unvalidated {field} was silently repaired"
        );
        assert!(client.last_event_at().is_none());
    }
}

fn near_budget_snapshot() -> ExceptionSnapshot {
    ExceptionSnapshot::new("Primary", "primary failure")
        .with_stack(&"a".repeat(16384), TraceOrigin::RuntimeReported)
        .with_cause(
            &ExceptionSnapshot::new("Cause", "cause")
                .with_stack(&"b".repeat(15800), TraceOrigin::RuntimeReported),
        )
}
fn reference() -> Value {
    json!({"version":1,"failure_ref":"11111111-1111-4111-8111-111111111111","scope":{"kind":"operation","operation_id":"22222222-2222-4222-8222-222222222222"}})
}
fn add_over_budget_evidence(event: &mut Value) {
    event["payload"]["exception_evidence"] = near_budget_snapshot().evidence().clone();
    assert!(wire::valid_event(event), "graph alone must be admissible");
    event["context"]["evidence_reference"] = reference();
    let additions = json!({"exception_evidence":event["payload"]["exception_evidence"],"evidence_reference":reference()});
    assert!(wire::canonical_size(&additions).unwrap() > 32768);
    assert!(
        !wire::valid_event(event),
        "combined budget must still apply"
    );
}

#[test]
fn combined_evidence_budget_reduces_optional_data_before_discarding_the_primary() {
    let corpus: Value =
        serde_json::from_str(include_str!("fixtures/evidence-wire-cases.json")).unwrap();
    let mut event = corpus["events"][0]["event"].clone();
    event["context"] = json!({});
    add_over_budget_evidence(&mut event);
    let protected = wire::protect_event(&Privacy::new(&[]).unwrap(), &event)
        .expect("optional evidence budget must not discard an otherwise valid primary");
    assert!(wire::valid_event(&protected));
    assert_eq!(protected["event_id"], event["event_id"]);
    assert_eq!(protected["payload"]["message"], event["payload"]["message"]);
    assert!(protected["payload"].get("exception_evidence").is_none());
    assert!(protected["context"].get("evidence_reference").is_none());
    assert_eq!(
        protected["context"]["evidence_limits"]["exception_evidence"],
        "budget_exceeded"
    );
}

#[test]
fn combined_evidence_budget_keeps_owner_hook_replacement_instead_of_restoring_original() {
    let recorder = Arc::new(Recorder::default());
    let mut config = configured(recorder.clone());
    config.before_send = Some(Arc::new(|mut event| {
        event["payload"]["message"] = json!("owner-approved replacement");
        add_over_budget_evidence(&mut event);
        Some(event)
    }));
    let client = Client::new(config);
    emit_until(
        &client,
        || {
            client.capture_exception(
                &ExceptionSnapshot::new("Primary", "original must stay removed"),
                &Context::default(),
            )
        },
        || !recorder.events_without_pressure().is_empty(),
    );
    let events = recorder.events_without_pressure();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0]["payload"]["message"],
        "owner-approved replacement"
    );
    assert!(events[0]["payload"].get("exception_evidence").is_none());
    assert!(events[0]["context"].get("evidence_reference").is_none());
    assert!(wire::valid_event(&events[0]));
}

#[test]
fn owner_hook_matches_shared_byte_and_malformed_boundaries() {
    let corpus: Value =
        serde_json::from_str(include_str!("fixtures/before-send-budget.json")).unwrap();
    for fixture in corpus["cases"].as_array().unwrap() {
        let recorder = Arc::new(Recorder::default());
        let mut config = configured(recorder.clone());
        config.evidence_capabilities = Some(json!({"version":1,"features":wire::FEATURES}));
        let replacement = fixture["event"].clone();
        config.before_send = Some(Arc::new(move |_| Some(replacement.clone())));
        let client = Client::new(config);
        emit_until(
            &client,
            || {
                client.capture_exception(
                    &ExceptionSnapshot::new("Primary", "owner removed"),
                    &Context::default(),
                )
            },
            || !recorder.events_without_pressure().is_empty(),
        );
        let events = recorder.events_without_pressure();
        assert_eq!(events.len(), 1, "{}", fixture["id"]);
        let event = &events[0];
        let fallback = fixture["outcome"] == "fallback";
        assert_eq!(
            event["payload"]["message"],
            if fallback {
                "owner removed"
            } else {
                "owner replacement"
            },
            "{}",
            fixture["id"]
        );
        assert!(wire::valid_event(event), "{}", fixture["id"]);
        if !fallback {
            assert_eq!(
                event["payload"].get("exception_evidence").is_some(),
                fixture["outcome"] == "keep",
                "{}",
                fixture["id"]
            );
            assert_eq!(event["context"]["owner_field"], "preserved");
        }
    }
}
