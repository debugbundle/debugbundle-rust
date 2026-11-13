use super::*;
use crate::runtime::{CallbackWorker, CLIENTS};
use std::time::Duration;

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
