use super::*;
use crate::{
    delivery::{Transport, TransportFailure, TransportResponse},
    Config,
};
use serde_json::Value;
use std::{future::Future, sync::mpsc, task::Poll};

struct UnusedTransport;
impl Transport for UnusedTransport {
    fn send(&self, _: &[u8], _: Duration) -> Result<TransportResponse, TransportFailure> {
        panic!("ACK persistence must not resend an event");
    }
}

#[tokio::test]
async fn canonical_ack_persistence_waits_for_bounded_callback_capacity() {
    exercise_persistence_backpressure(false, false).await;
}

#[tokio::test]
async fn canonical_ack_ownership_survives_a_persistence_timeout() {
    exercise_persistence_backpressure(true, false).await;
}

#[tokio::test]
async fn shutdown_does_not_wait_for_a_stalled_persistence_lane() {
    exercise_persistence_backpressure(false, true).await;
}

async fn exercise_persistence_backpressure(expire: bool, stop: bool) {
    let root = tempfile::tempdir().unwrap();
    let client = Client::new(Config {
        spool_dir: root.path().join("spool"),
        transport: Some(Arc::new(UnusedTransport)),
        request_timeout: if expire {
            Duration::from_millis(20)
        } else {
            Duration::from_secs(5)
        },
        ..Config::new("synthetic", "checkout")
    });
    let inner = client.inner.as_ref().unwrap().clone();
    let store = Store::open(&inner.config.spool_dir).unwrap();
    let corpus: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/relay-compliance.json"
    ))
    .unwrap();
    let event = corpus["cases"][0]["request"]["bodyJson"]["batch"][0].clone();
    let now = chrono::Utc::now().timestamp_millis();
    store.admit(vec![event], &scope(&inner), &[], now).unwrap();
    let mut work = store
        .scan(
            &scope(&inner),
            now,
            spool::LeaseControl {
                reservation: inner.spool_lease.clone(),
                slot: inner.slot.clone(),
                cleanup: inner.callbacks.native_cleanup(),
            },
        )
        .unwrap()
        .work
        .unwrap();
    work.record.states[0] = Disposition::Accepted;
    work.record.acknowledged_digests[0] =
        Some(spool::digest(&serde_json::to_vec(&work.events[0]).unwrap()));
    let previous = work.metadata.clone();

    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = mpsc::channel();
    let blocked_inner = inner.clone();
    let blocked = tokio::spawn(async move {
        blocked_inner
            .callbacks
            .call(move || {
                started.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(3)).unwrap();
            })
            .await
            .unwrap();
    });
    ready.await.unwrap();
    let queued = inner.callbacks.call(|| ());
    tokio::pin!(queued);
    assert!(
        std::future::poll_fn(|cx| Poll::Ready(queued.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    let (waited, persisted) = {
        let persistence = persist(&inner, &mut work);
        tokio::pin!(persistence);
        let initial = std::future::poll_fn(|cx| Poll::Ready(persistence.as_mut().poll(cx))).await;
        if expire {
            assert!(
                tokio::time::timeout(Duration::from_millis(80), persistence.as_mut())
                    .await
                    .is_err()
            );
            assert!(inner.spool_lease.pending.load(Ordering::Acquire));
        }
        if stop {
            inner.stopped.store(true, Ordering::Release);
            inner.stop.notify_waiters();
            assert!(
                !tokio::time::timeout(Duration::from_millis(200), persistence.as_mut())
                    .await
                    .unwrap()
            );
        }
        release.send(()).unwrap();
        blocked.await.unwrap();
        queued.await.unwrap();
        let persisted = if stop {
            false
        } else {
            match initial {
                Poll::Pending => persistence.await,
                Poll::Ready(value) => value,
            }
        };
        (initial.is_pending(), persisted)
    };
    assert!(
        waited,
        "a full callback queue discarded the canonical ACK update"
    );
    assert_eq!(persisted, !stop);
    assert_eq!(work.metadata != previous, !stop);
    assert_eq!(
        inner.spool_status.lock().unwrap().storage_failures > 0,
        expire
    );
    if !stop {
        assert!(
            forward(&inner, work).await,
            "accepted progress must finish without calling the transport again"
        );
    }
}
