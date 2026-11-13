mod helpers;
use debugbundle::{
    Client, Context, ExceptionSnapshot, OperationOptions, RecoveryObservation, RecoveryState,
};
use helpers::{configured, Recorder};
use serde_json::json;
use std::{sync::Arc, time::Duration};
const FAILURE: &str = "11111111-1111-4111-8111-111111111111";
const ATTEMPT: &str = "22222222-2222-4222-8222-222222222222";
fn observation(state: RecoveryState) -> RecoveryObservation {
    RecoveryObservation {
        failure_ref: FAILURE.into(),
        attempt_id: ATTEMPT.into(),
        state,
        operation: Some("checkout.retry".into()),
        status_code: Some(200),
        ..Default::default()
    }
}
fn client(recorder: Arc<Recorder>) -> Client {
    let mut config = configured(recorder);
    config.capture_recovery_observations = true;
    config.evidence_capabilities = Some(
        json!({"version":1,"features":["exception_evidence_v1","recovery_v1","evidence_provenance_v1"]}),
    );
    Client::new(config)
}

#[test]
fn recovery_is_explicit_and_uses_the_owned_failure_scope() {
    let recorder = Arc::new(Recorder::default());
    let client = client(recorder.clone());
    let operation = client.start_operation(OperationOptions::default());
    let context = operation.context().with_failure_ref(FAILURE);
    client.capture_exception(&ExceptionSnapshot::new("Failure", "failed"), &context);
    client.capture_recovery(&observation(RecoveryState::AttemptCompleted), &context);
    client.capture_recovery(&observation(RecoveryState::RecoveryObserved), &context);
    assert!(client.flush(Duration::from_secs(2)).drained);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 3);
    let failure = events
        .iter()
        .find(|event| event["event_type"] == "backend_exception")
        .unwrap();
    let recovery = events
        .iter()
        .filter(|event| event["event_type"] == "diagnostic_context")
        .collect::<Vec<_>>();
    assert_eq!(
        failure["context"]["evidence_reference"]["failure_ref"],
        FAILURE
    );
    assert_eq!(recovery[0]["payload"]["state"], "attempt_completed");
    assert_eq!(recovery[1]["payload"]["state"], "recovery_observed");
    assert_eq!(
        recovery[0]["payload"]["scope"]["operation_id"],
        operation.handle().operation_id().unwrap()
    );
}

#[test]
fn recovery_is_off_without_local_opt_in_or_proof_and_stale_owners_cannot_admit() {
    for mode in 0..2 {
        let recorder = Arc::new(Recorder::default());
        let mut config = configured(recorder.clone());
        config.capture_recovery_observations = mode == 0;
        if mode == 1 {
            config.evidence_capabilities = Some(json!({"version":1,"features":["recovery_v1"]}));
        }
        let client = Client::new(config);
        let operation = client.start_operation(OperationOptions::default());
        client.capture_recovery(
            &observation(RecoveryState::AttemptStarted),
            &operation.context(),
        );
        assert!(client.flush(Duration::from_secs(1)).drained);
        assert!(recorder.events.lock().unwrap().is_empty());
    }
    let recorder = Arc::new(Recorder::default());
    let client = client(recorder.clone());
    let operation = client.start_operation(OperationOptions::default());
    let stale = operation.context();
    drop(operation);
    client.capture_recovery(&observation(RecoveryState::AttemptStarted), &stale);
    client.capture_recovery(
        &observation(RecoveryState::AttemptStarted),
        &Context::default(),
    );
    assert!(client.flush(Duration::from_secs(1)).drained);
    assert!(recorder.events.lock().unwrap().is_empty());
}

#[test]
fn every_recovery_state_preserves_explicit_page_scope_and_timing() {
    let recorder = Arc::new(Recorder::default());
    let client = client(recorder.clone());
    let context = Context::default()
        .with_trace_id(FAILURE)
        .with_session_id("33333333-3333-4333-8333-333333333333");
    let at = chrono::Utc::now() - chrono::Duration::milliseconds(100);
    for (state, expected) in [
        (RecoveryState::AttemptStarted, "attempt_started"),
        (RecoveryState::AttemptCompleted, "attempt_completed"),
        (RecoveryState::AttemptFailed, "attempt_failed"),
        (RecoveryState::RecoveryObserved, "recovery_observed"),
        (RecoveryState::FailurePersisted, "failure_persisted"),
    ] {
        let mut value = observation(state);
        value.scope = Some(debugbundle::EvidenceScope::Page {
            page_id: ATTEMPT.into(),
        });
        value.duration = Some(Duration::from_millis(123));
        value.occurred_at = Some(at);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            client.capture_recovery(&value, &context);
            assert!(client.flush(Duration::from_secs(1)).drained);
            if recorder
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|event| event["payload"]["state"] == expected)
            {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
    }
    let events = recorder.events.lock().unwrap();
    let recovery = events
        .iter()
        .filter(|event| event["event_type"] == "diagnostic_context")
        .collect::<Vec<_>>();
    assert_eq!(recovery.len(), 5);
    for event in recovery {
        assert_eq!(
            event["payload"]["scope"],
            json!({"kind":"page","page_id":ATTEMPT})
        );
        assert_eq!(event["payload"]["duration_ms"].as_f64(), Some(123.0));
        assert_eq!(
            event["occurred_at"],
            at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        );
    }
}

#[test]
fn ack_releases_bytes_without_refunding_observation_allowances() {
    use std::time::Instant;
    let recorder = Arc::new(Recorder::default());
    let client = client(recorder.clone());
    let operation = client.start_operation(OperationOptions::default());
    let count = || {
        recorder
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| {
                event["event_type"] == "diagnostic_context"
                    && event["payload"]["kind"] == "recovery"
            })
            .count()
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while count() < 20 {
        client.capture_recovery(
            &observation(RecoveryState::AttemptStarted),
            &operation.context(),
        );
        assert!(client.flush(Duration::from_secs(1)).drained);
        assert!(
            Instant::now() < deadline,
            "recovery admission never reached its allowance"
        );
        std::thread::yield_now();
    }
    for _ in 0..5 {
        client.capture_recovery(
            &observation(RecoveryState::AttemptStarted),
            &operation.context(),
        );
        assert!(client.flush(Duration::from_secs(1)).drained);
    }
    assert_eq!(count(), 20, "ACK must not refund recovery allowances");
    // Contended best-effort calls can additionally emit operational pressure
    // summaries. Those are not recovery observations and consume no allowance.
    assert!(recorder
        .events
        .lock()
        .unwrap()
        .iter()
        .all(|event| event["event_type"] == "diagnostic_context"
            || (event["event_type"] == "error_suppressed"
                && event["payload"]["reason"] == "queue_pressure")));
}

#[test]
fn valid_hook_identity_replacements_preserve_diagnostic_ownership() {
    let recorder = Arc::new(Recorder::default());
    let mut config = configured(recorder.clone());
    config.capture_recovery_observations = true;
    config.evidence_capabilities = Some(json!({"version":1,"features":["recovery_v1"]}));
    config.before_send = Some(Arc::new(|mut event| {
        event["event_id"] = json!("33333333-3333-4333-8333-333333333333");
        Some(event)
    }));
    let client = Client::new(config);
    let operation = client.start_operation(OperationOptions::default());
    client.capture_recovery(
        &observation(RecoveryState::AttemptStarted),
        &operation.context(),
    );
    assert!(client.flush(Duration::from_secs(2)).drained);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0]["event_id"],
        "33333333-3333-4333-8333-333333333333"
    );
}
