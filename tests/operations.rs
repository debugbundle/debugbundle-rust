mod helpers;
use debugbundle::{Client, Context, ExceptionSnapshot, OperationKind, OperationOptions};
use helpers::{configured, Recorder};
use std::{sync::Arc, time::Duration};

fn setup() -> (Client, Arc<Recorder>) {
    let recorder = Arc::new(Recorder::default());
    let mut config = configured(recorder.clone());
    config.evidence_capabilities = Some(debugbundle::wire::descriptor(&[
        "exception_evidence_v1".into(),
        "evidence_provenance_v1".into(),
    ]));
    (Client::new(config), recorder)
}

#[test]
fn operation_context_uses_real_elapsed_time_and_revokes_stale_handles() {
    let (client, recorder) = setup();
    let operation = client.start_operation(OperationOptions {
        kind: OperationKind::Request,
        name: "/checkout".into(),
        limit: Some(Duration::from_secs(5)),
        ..OperationOptions::default()
    });
    client.capture_exception(
        &ExceptionSnapshot::new("Active", "failure"),
        &operation.context(),
    );
    let stale = operation.context();
    drop(operation);
    client.capture_exception(&ExceptionSnapshot::new("Stale", "failure"), &stale);
    assert!(client.flush(Duration::from_secs(2)).drained);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events[0]["payload"]["execution"]["kind"], "request");
    assert_eq!(events[0]["payload"]["execution"]["name"], "/checkout");
    assert_eq!(events[0]["payload"]["execution"]["limit_ms"], 5000.0);
    assert!(
        events[0]["payload"]["execution"]["elapsed_ms"]
            .as_f64()
            .unwrap()
            >= 0.0
    );
    assert!(events[1]["payload"].get("execution").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn poll_scoped_context_is_isolated_across_interleaved_tasks() {
    let (client, recorder) = setup();
    let first = client.start_operation(OperationOptions {
        name: "first".into(),
        ..OperationOptions::default()
    });
    let second = client.start_operation(OperationOptions {
        name: "second".into(),
        ..OperationOptions::default()
    });
    let a = client.clone();
    let b = client.clone();
    // Admission may deliberately drop on a contended SDK lock; serialize capture
    // while still interleaving future polls so this test isolates context ownership.
    let admission = Arc::new(tokio::sync::Semaphore::new(1));
    let first_admission = admission.clone();
    let left = tokio::spawn(first.scope(async move {
        tokio::task::yield_now().await;
        let _permit = first_admission.acquire().await.unwrap();
        a.capture_exception(
            &ExceptionSnapshot::new("First", "failure"),
            &Context::default(),
        );
    }));
    let right = tokio::spawn(second.scope(async move {
        tokio::task::yield_now().await;
        let _permit = admission.acquire().await.unwrap();
        b.capture_exception(
            &ExceptionSnapshot::new("Second", "failure"),
            &Context::default(),
        );
    }));
    left.await.unwrap();
    right.await.unwrap();
    assert!(client.flush_async(Duration::from_secs(2)).await.drained);
    {
        let events = recorder.events.lock().unwrap();
        assert_eq!(events.len(), 2);
        for event in &*events {
            let expected = if event["payload"]["name"] == "First" {
                "first"
            } else {
                "second"
            };
            assert_eq!(event["payload"]["execution"]["name"], expected);
        }
    }
    client.capture_exception(
        &ExceptionSnapshot::new("Outside", "failure"),
        &Context::default(),
    );
    assert!(client.flush_async(Duration::from_secs(2)).await.drained);
}

#[test]
fn parent_completion_revokes_descendants_and_cross_client_scopes() {
    let (client, recorder) = setup();
    let parent = client.start_operation(OperationOptions::default());
    let child = client.start_operation(OperationOptions {
        parent: Some(parent.handle()),
        ..OperationOptions::default()
    });
    assert!(child.is_active());
    drop(parent);
    assert!(!child.is_active());
    let invalid = client.start_operation(OperationOptions {
        parent: Some(child.handle()),
        ..OperationOptions::default()
    });
    assert!(!invalid.is_active());
    let (other, _) = setup();
    other.capture_exception(
        &ExceptionSnapshot::new("Foreign", "failure"),
        &child.context(),
    );
    assert!(other.flush(Duration::from_secs(2)).drained);
    assert!(recorder.events.lock().unwrap().is_empty());
}

#[test]
fn completed_owners_release_the_bounded_registry_even_when_contexts_are_retained() {
    let (client, _) = setup();
    let mut operations = Vec::new();
    for _ in 0..1024 {
        let operation = client.start_operation(OperationOptions::default());
        assert!(operation.is_active());
        operations.push(operation);
    }
    assert!(!client
        .start_operation(OperationOptions::default())
        .is_active());
    let stale = operations.pop().unwrap().context();
    assert!(client
        .start_operation(OperationOptions::default())
        .is_active());
    assert!(!stale.operation_active());
}
