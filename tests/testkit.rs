#![cfg(feature = "testkit")]
use debugbundle::{
    delivery::Transport, testkit::RecordingTransport, Client, Config, Context, Level,
};
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

// Snapshot observation is explicitly non-waiting: the sender may briefly own
// state even after flush returns. Wait only for availability, never for desired
// accounting values, so every assertion below still checks the first snapshot.
fn resource_snapshot(client: &Client) -> debugbundle::testkit::ResourceSnapshot {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(snapshot) = debugbundle::testkit::resources(client) {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "resource snapshot remained unavailable"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn resource_snapshots_preserve_queue_and_recovery_accounting_across_ack_and_owner_stop() {
    use debugbundle::{ExceptionSnapshot, OperationOptions, RecoveryObservation, RecoveryState};
    assert!(debugbundle::testkit::resources(&Client::new(Config {
        enabled: false,
        ..Default::default()
    }))
    .is_none());
    let recorder = Arc::new(RecordingTransport::new(10).unwrap());
    let client = Client::new(Config {
        transport: Some(recorder),
        batch_size: 100,
        flush_interval: Duration::from_secs(300),
        capture_recovery_observations: true,
        evidence_capabilities: Some(json!({"version":1,"features":debugbundle::wire::FEATURES})),
        ..Config::new("synthetic", "test-resources")
    });
    let mut operation = client.start_operation(OperationOptions::default());
    let failure = "11111111-1111-4111-8111-111111111111";
    let context = operation.context().with_failure_ref(failure);
    client.capture_exception(
        &ExceptionSnapshot::new("SyntheticFailure", "safe"),
        &context,
    );
    client.capture_recovery(
        &RecoveryObservation {
            failure_ref: failure.into(),
            attempt_id: "22222222-2222-4222-8222-222222222222".into(),
            state: RecoveryState::AttemptStarted,
            ..Default::default()
        },
        &context,
    );
    let before = resource_snapshot(&client);
    assert_eq!(before.buffered_events, 2);
    assert_eq!(before.pending_diagnostics, 1);
    assert_eq!(before.diagnostic_windows, 1);
    assert!(before.diagnostic_bytes > 0 && before.diagnostic_bytes <= 65536);
    assert_eq!(before.operation_slots, 1);
    assert!(before.operation_metadata_bytes > 0);
    assert!((1..=8).contains(&before.process_client_slots));
    assert!((1..=8).contains(&before.process_callback_workers));
    assert_eq!(client.flush(Duration::from_secs(2)).accepted_events, 2);
    let after = resource_snapshot(&client);
    assert_eq!(after.buffered_events, 0);
    assert_eq!(after.pending_diagnostics, 0);
    assert_eq!(after.diagnostic_windows, 1);
    assert!(after.diagnostic_bytes < before.diagnostic_bytes);
    operation.stop();
    assert!(!context.operation_active());
    assert_eq!(resource_snapshot(&client).operation_slots, 0);
    drop(context);
    drop(operation);
    assert_eq!(resource_snapshot(&client).operation_slots, 0);
}
#[test]
fn recording_transport_exercises_public_capture_retry_privacy_and_drain() {
    let recorder = Arc::new(RecordingTransport::new(4).unwrap());
    recorder.fail_next(1);
    let client = Client::new(Config {
        transport: Some(recorder.clone()),
        ..Config::new("synthetic", "testkit")
    });
    client.capture_log(
        Level::Error,
        "password=SYNTHETIC_SECRET",
        &Context::default(),
    );
    let outcome = client.flush(Duration::from_secs(3));
    assert!(outcome.drained);
    assert_eq!(outcome.accepted_events, 1);
    assert_eq!(recorder.attempts(), 2);
    let events = recorder.drain();
    assert_eq!(events.len(), 1);
    assert!(debugbundle::wire::valid_event(&events[0]));
    assert!(!serde_json::to_string(&events)
        .unwrap()
        .contains("SYNTHETIC_SECRET"));
    assert!(recorder.events().is_empty());
}
#[test]
fn recording_transport_rejects_invalid_or_full_batches_without_partial_acceptance() {
    assert!(RecordingTransport::new(0).is_none());
    assert!(RecordingTransport::new(1001).is_none());
    let recorder = RecordingTransport::new(1).unwrap();
    assert!(recorder
        .send(br#"{"events":[false]}"#, Duration::from_secs(1))
        .is_err());
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/relay-compliance.json")).unwrap();
    let mut event = fixture["cases"][0]["request"]["bodyJson"]["batch"][0].clone();
    event["sdk_name"] = json!("@debugbundle/sdk-browser");
    let body = serde_json::to_vec(&json!({"events":[event]})).unwrap();
    assert_eq!(
        recorder.send(&body, Duration::from_secs(1)).unwrap().status,
        202
    );
    assert!(recorder.send(&body, Duration::from_secs(1)).is_err());
    assert_eq!(recorder.events().len(), 1);
}
