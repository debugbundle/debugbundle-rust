mod helpers;
use debugbundle::{privacy::Privacy, wire, Client, Context, ExceptionSnapshot, TraceOrigin};
use helpers::{configured, emit_until, Recorder};
use serde_json::{json, Value};
use std::sync::Arc;

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
