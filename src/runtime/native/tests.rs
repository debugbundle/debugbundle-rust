use super::*;
use crate::runtime::{CallbackWorker, CLIENTS};
use std::time::Duration;

fn locked_file() -> (tempfile::NamedTempFile, File, File) {
    let anchor = tempfile::NamedTempFile::new().unwrap();
    let file = anchor.reopen().unwrap();
    let witness = anchor.reopen().unwrap();
    file.try_lock().unwrap();
    assert!(matches!(
        witness.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    (anchor, file, witness)
}

#[tokio::test]
async fn native_persistence_never_runs_on_a_poisoned_or_disconnected_lane() {
    for poisoned in [true, false] {
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(receiver);
        let worker = CallbackWorker {
            sender,
            poisoned: Arc::new(AtomicBool::new(poisoned)),
            capacity: Arc::new(tokio::sync::Notify::new()),
        };
        assert!(matches!(
            worker
                .persist_native(|| panic!("unavailable lane executed native work"))
                .await,
            Err(CallbackFailure::Unavailable)
        ));
    }
}

#[tokio::test]
async fn file_cleanup_on_its_callback_lane_releases_the_lease_before_returning() {
    assert!(crate::runtime::claim(&CLIENTS));
    let slot = Arc::new(ClientSlot);
    let worker = CallbackWorker::new(slot.clone()).unwrap();
    let native = worker.native_cleanup();
    let (_anchor, file, witness) = locked_file();
    let reservation = Arc::new(FileReservation::default());
    reservation.pending.store(true, Ordering::Release);
    let released = reservation.released.notified();
    tokio::pin!(released);
    released.as_mut().enable();
    let pending = reservation.clone();
    worker
        .call(move || {
            assert!(crate::runtime::in_callback());
            native.close(file, pending.clone(), slot);
            assert!(!pending.pending.load(Ordering::Acquire));
            witness.try_lock().unwrap();
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), released)
        .await
        .expect("inline native close must notify existing reservation waiters");
}

#[tokio::test]
async fn disconnected_callback_lane_defers_file_close_and_releases_its_client_slot() {
    assert!(crate::runtime::claim(&CLIENTS));
    let slot = Arc::new(ClientSlot);
    let weak_slot = Arc::downgrade(&slot);
    let (sender, receiver) = mpsc::sync_channel(1);
    drop(receiver);
    let native = NativeCleanup {
        sender,
        capacity: Arc::new(tokio::sync::Notify::new()),
    };
    let (_anchor, file, witness) = locked_file();
    let reservation = Arc::new(FileReservation::default());
    reservation.pending.store(true, Ordering::Release);
    let released = reservation.released.notified();
    tokio::pin!(released);
    released.as_mut().enable();
    native.close(file, reservation.clone(), slot);
    tokio::time::timeout(Duration::from_secs(2), released)
        .await
        .expect("disconnected native close must release its reservation");
    assert!(!reservation.pending.load(Ordering::Acquire));
    witness.try_lock().unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while weak_slot.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fallback close must retain then release the client capacity permit");
}

#[tokio::test]
async fn file_cleanup_waits_for_its_callback_lane_without_joining_stalled_app_cleanup() {
    assert!(crate::runtime::claim(&CLIENTS));
    let slot = Arc::new(ClientSlot);
    let worker = CallbackWorker::new(slot.clone()).unwrap();
    let native = worker.native_cleanup();
    let (global_started, global_ready) = mpsc::channel();
    let (global_release, global_wait) = mpsc::channel();
    crate::runtime::enqueue_cleanup(
        Box::new(move || {
            global_started.send(()).unwrap();
            let _ = global_wait.recv_timeout(Duration::from_secs(3));
        }),
        slot.clone(),
    );
    global_ready.recv_timeout(Duration::from_secs(1)).unwrap();

    let (callback_started, callback_ready) = mpsc::channel();
    let (callback_release, callback_wait) = mpsc::channel();
    worker
        .sender
        .try_send(Box::new(move || {
            callback_started.send(()).unwrap();
            let _ = callback_wait.recv_timeout(Duration::from_secs(3));
        }))
        .ok()
        .unwrap();
    callback_ready.recv_timeout(Duration::from_secs(1)).unwrap();
    // Occupy the one queued callback slot in addition to the executing one.
    worker.sender.try_send(Box::new(|| {})).ok().unwrap();
    let reservation = Arc::new(FileReservation::default());
    reservation.pending.store(true, Ordering::Release);
    native.close(
        tempfile::tempfile().unwrap(),
        reservation.clone(),
        slot.clone(),
    );
    tokio::task::yield_now().await;
    assert!(reservation.pending.load(Ordering::Acquire));
    callback_release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while reservation.pending.load(Ordering::Acquire) {
            let notified = reservation.released.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !reservation.pending.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
    })
    .await
    .expect("native cleanup must not wait for application cleanup");
    global_release.send(()).unwrap();
}
