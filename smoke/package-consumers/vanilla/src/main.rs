use debugbundle::{
    Client, Config, Context, ExceptionInput, ExceptionSnapshot, OperationKind, OperationOptions,
    ProbeOptions, RecoveryObservation, RecoveryState, TraceOrigin,
};
use serde_json::{json, Value};
use std::time::Duration;
use tracing_subscriber::prelude::*;
#[derive(Debug, thiserror::Error)]
#[error("application-owned failure")]
struct Failure;
fn main() {
    let mock = artifact_support::Mock::start();
    let directory = tempfile::tempdir().unwrap();
    let client = mock.client("artifact-vanilla", directory.path().join("spool"));
    mock.wait_config(&client);
    let subscriber = tracing_subscriber::registry().with(client.tracing_layer());
    let context = Context::default().with_trace_id(artifact_support::TRACE);
    let error: anyhow::Error = Failure.into();
    client.capture_error(ExceptionInput::borrowed(&*error), &context);
    client.capture_exception(
        &ExceptionSnapshot::new("SyntheticFailure", "password=BROWSER_SECRET"),
        &context,
    );
    tracing::subscriber::with_default(subscriber, || {
        tracing::error!(technical = 7, "artifact native tracing")
    });
    let outcome = client.flush(Duration::from_secs(5));
    assert!(outcome.drained);
    assert_eq!(outcome.accepted_events, 3);
    let events = mock.events();
    assert_eq!(events.len(), 3);
    for event in &events {
        assert_eq!(event["service"]["name"], "artifact-vanilla");
        assert_eq!(event["service"]["environment"], "artifact-test");
        assert_eq!(event["sdk_name"], "@debugbundle/sdk-rust");
    }
    assert!(!serde_json::to_string(&events)
        .unwrap()
        .contains("BROWSER_SECRET"));
    assert!(client.shutdown(Duration::from_secs(1)).drained);
    local_handoff();
    println!("vanilla exact-artifact HTTP/error/tracing/privacy acceptance passed");
}

fn local_handoff() {
    let directory = tempfile::tempdir().unwrap();
    let events_dir = directory.path().join("events");
    let client = Client::new(Config {
        environment: "artifact-test".into(),
        capture_recovery_observations: true,
        evidence_capabilities: Some(json!({"version":1,"features":debugbundle::wire::FEATURES})),
        ..Config::local("artifact-local-rust", &events_dir)
    });
    let operation = client.start_operation(OperationOptions {
        kind: OperationKind::Command,
        name: "checkout.retry".into(),
        limit: Some(Duration::from_secs(10)),
        ..Default::default()
    });
    let context = operation
        .context()
        .with_trace_id(artifact_support::TRACE)
        .with_failure_ref("11111111-1111-4111-8111-111111111111");
    client.probe(
        "checkout.state",
        &json!({"phase":"authorize", "password":"BROWSER_SECRET"}),
        ProbeOptions::default(),
        &context,
    );
    let failure = ExceptionSnapshot::new("CheckoutFailure", "payment failed")
        .with_stack(
            "0: checkout::authorize\n   at /srv/src/checkout.rs:42:7",
            TraceOrigin::RuntimeReported,
        )
        .with_cause(&ExceptionSnapshot::new(
            "StorageFailure",
            "password=BROWSER_SECRET",
        ));
    client.capture_exception(&failure, &context);
    for state in [
        RecoveryState::AttemptCompleted,
        RecoveryState::RecoveryObserved,
    ] {
        client.capture_recovery(
            &RecoveryObservation {
                failure_ref: "11111111-1111-4111-8111-111111111111".into(),
                attempt_id: "22222222-2222-4222-8222-222222222222".into(),
                state,
                operation: Some("checkout.retry".into()),
                status_code: Some(200),
                ..Default::default()
            },
            &context,
        );
    }
    assert_eq!(client.flush(Duration::from_secs(5)).accepted_events, 3);
    let mut events = Vec::<Value>::new();
    for file in std::fs::read_dir(&events_dir).unwrap() {
        let batch: Vec<Value> =
            serde_json::from_slice(&std::fs::read(file.unwrap().path()).unwrap()).unwrap();
        events.extend(batch);
    }
    assert_eq!(events.len(), 3);
    let failure = events
        .iter()
        .find(|event| event["event_type"] == "backend_exception")
        .unwrap();
    assert_eq!(
        failure["payload"]["exception_evidence"]["runtime"],
        "unknown"
    );
    assert_eq!(
        failure["payload"]["exception_evidence"]["nodes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(failure["payload"].get("probe_data").is_some());
    let bytes = serde_json::to_vec_pretty(&events).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("BROWSER_SECRET"));
    if let Some(output) = std::env::var_os("DEBUGBUNDLE_ARTIFACT_EVENTS_OUTPUT") {
        std::fs::write(output, bytes).unwrap();
    }
    assert!(client.shutdown(Duration::from_secs(1)).drained);
    println!("vanilla exact-artifact local/graph/probe/recovery handoff passed");
}
