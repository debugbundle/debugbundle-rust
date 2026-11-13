//! Native file cleanup uses the owning callback lane, isolated from app destructors.
use super::{ClientSlot, Job};
use std::{
    fs::File,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
};
#[derive(Default)]
pub(crate) struct FileReservation {
    pub pending: AtomicBool,
    pub released: tokio::sync::Notify,
}

#[cfg(test)]
mod tests;
#[derive(Clone)]
pub(crate) struct NativeCleanup {
    pub(super) sender: mpsc::SyncSender<Job>,
    pub(super) capacity: Arc<tokio::sync::Notify>,
}
struct PendingFile {
    file: Option<File>,
    reservation: Arc<FileReservation>,
    _slot: Arc<ClientSlot>,
}
impl Drop for PendingFile {
    fn drop(&mut self) {
        drop(self.file.take());
        self.reservation.pending.store(false, Ordering::Release);
        self.reservation.released.notify_waiters();
    }
}
impl NativeCleanup {
    pub(crate) fn close(
        &self,
        file: File,
        reservation: Arc<FileReservation>,
        slot: Arc<ClientSlot>,
    ) {
        let pending = PendingFile {
            file: Some(file),
            reservation,
            _slot: slot.clone(),
        };
        // Queued job disposal also closes the file and releases its reservation.
        let mut job: Job = Box::new(move || drop(pending));
        if super::in_callback() {
            job();
            return;
        }
        let Some(runtime) = super::handle() else {
            super::enqueue_cleanup(job, slot);
            return;
        };
        let sender = self.sender.clone();
        let capacity = self.capacity.clone();
        runtime.spawn(async move {
            loop {
                let ready = capacity.notified();
                tokio::pin!(ready);
                ready.as_mut().enable();
                match sender.try_send(job) {
                    Ok(()) => return,
                    Err(mpsc::TrySendError::Full(returned)) => {
                        job = returned;
                        ready.await;
                    }
                    Err(mpsc::TrySendError::Disconnected(returned)) => {
                        super::enqueue_cleanup(returned, slot);
                        return;
                    }
                }
            }
        });
    }
}
