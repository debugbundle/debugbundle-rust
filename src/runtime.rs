//! Fixed process resources. A stalled application callback never gets a replacement.
use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc, OnceLock,
    },
    time::Duration,
};
use tokio::runtime::Handle;
#[cfg(feature = "local")]
mod native;
#[cfg(feature = "local")]
pub(crate) use native::{FileReservation, NativeCleanup};
#[cfg(test)]
mod tests;
thread_local! {static INTERNAL:std::cell::Cell<bool>=const{std::cell::Cell::new(false)};static CALLBACK:std::cell::Cell<bool>=const{std::cell::Cell::new(false)};}
pub(crate) fn internal() -> bool {
    INTERNAL.with(std::cell::Cell::get)
}
pub(crate) fn in_callback() -> bool {
    CALLBACK.with(std::cell::Cell::get)
}
struct CallbackGuard;
impl Drop for CallbackGuard {
    fn drop(&mut self) {
        CALLBACK.with(|flag| flag.set(false));
    }
}
static RUNTIME: OnceLock<Option<Handle>> = OnceLock::new();
pub(crate) static CLIENTS: AtomicUsize = AtomicUsize::new(0);
static CALLBACKS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "testkit")]
pub(crate) fn resources() -> (usize, usize) {
    (
        CLIENTS.load(Ordering::Acquire),
        CALLBACKS.load(Ordering::Acquire),
    )
}
pub(crate) struct ClientSlot;
impl Drop for ClientSlot {
    fn drop(&mut self) {
        CLIENTS.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Cleanup {
    work: Box<dyn FnOnce() + Send>,
    _slot: Arc<ClientSlot>,
}
static CLEANUP: OnceLock<Option<mpsc::Sender<Cleanup>>> = OnceLock::new();
fn cleanup_sender() -> Option<&'static mpsc::Sender<Cleanup>> {
    CLEANUP
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Cleanup>();
            std::thread::Builder::new()
                .name("debugbundle-cleanup".into())
                .spawn(move || {
                    INTERNAL.with(|flag| flag.set(true));
                    while let Ok(job) = rx.recv() {
                        let Cleanup { work, _slot } = job;
                        if let Err(payload) =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                        {
                            discard_panic(payload);
                        }
                    }
                })
                .ok()?;
            Some(tx)
        })
        .as_ref()
}
fn dispose<T>(value: T) {
    if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(value))) {
        discard_panic(payload);
    }
}
struct InternalGuard(bool);
impl Drop for InternalGuard {
    fn drop(&mut self) {
        INTERNAL.with(|flag| flag.set(self.0));
    }
}
pub(crate) fn discard_configuration(mut config: crate::Config) {
    let _guard = InternalGuard(INTERNAL.with(|flag| flag.replace(true)));
    // Take every application-owned value before dropping any of them. Separate
    // unwind boundaries prevent two panicking destructors from double-panicking
    // during Config's automatic field cleanup.
    let hook = config.before_send.take();
    let transport = config.transport.take();
    #[cfg(feature = "local")]
    let limiter = config.relay_rate_limit_store.take();
    if let Some(proof) = config.evidence_capabilities.take() {
        crate::owned_json::discard(proof);
    }
    dispose(hook);
    dispose(transport);
    #[cfg(feature = "local")]
    dispose(limiter);
}
pub(crate) fn defer_configuration(config: crate::Config, slot: Arc<ClientSlot>) {
    enqueue_cleanup(Box::new(move || discard_configuration(config)), slot);
}
pub(crate) async fn cleanup_value<T: Send + 'static>(value: T, slot: Arc<ClientSlot>) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    enqueue_cleanup(
        Box::new(move || {
            dispose(value);
            let _ = tx.send(());
        }),
        slot,
    );
    let _ = rx.await;
}
fn enqueue_cleanup(work: Box<dyn FnOnce() + Send>, slot: Arc<ClientSlot>) {
    let job = Cleanup { work, _slot: slot };
    // Eight client permits bound pending producer and final configuration
    // disposal. Relay leases close on their owning callback lane. A stalled destructor cannot
    // refill those reservations or obtain a replacement cleanup thread.
    if let Some(sender) = cleanup_sender() {
        if let Err(orphan) = sender.send(job) {
            std::mem::forget(orphan);
        }
    } else {
        std::mem::forget(job);
    }
    // The static receiver never disconnects during normal operation. If runtime
    // creation fails, retain the bounded permit rather than running unknown
    // destructors on a request caller or replenishing its process capacity.
}

pub(crate) fn claim(counter: &AtomicUsize) -> bool {
    for _ in 0..32 {
        let n = counter.load(Ordering::Acquire);
        if n >= 8 {
            return false;
        }
        if counter
            .compare_exchange_weak(n, n + 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return true;
        }
    }
    false
}
pub(crate) fn handle() -> Option<Handle> {
    RUNTIME
        .get_or_init(|| {
            cleanup_sender()?;
            let (tx, rx) = mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("debugbundle-io".into())
                .spawn(move || {
                    INTERNAL.with(|flag| flag.set(true));
                    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .max_blocking_threads(8)
                        .thread_name("debugbundle-native")
                        .on_thread_start(|| INTERNAL.with(|flag| flag.set(true)))
                        .build()
                    else {
                        return;
                    };
                    if tx.send(runtime.handle().clone()).is_ok() {
                        runtime.block_on(std::future::pending::<()>());
                    }
                })
                .ok()?;
            rx.recv_timeout(Duration::from_secs(1)).ok()
        })
        .clone()
}
type Job = Box<dyn FnOnce() + Send>;
#[derive(Clone, Copy, Debug)]
pub(crate) enum CallbackFailure {
    Panicked,
    Unavailable,
}
pub(crate) struct CallbackWorker {
    sender: mpsc::SyncSender<Job>,
    poisoned: Arc<AtomicBool>,
    #[cfg(feature = "local")]
    capacity: Arc<tokio::sync::Notify>,
}
impl CallbackWorker {
    pub(crate) fn available(&self) -> bool {
        !self.poisoned.load(Ordering::Acquire)
    }
    pub(crate) fn new(slot: Arc<ClientSlot>) -> Option<Self> {
        if !claim(&CALLBACKS) {
            return None;
        }
        let (tx, rx) = mpsc::sync_channel::<Job>(1);
        let poisoned = Arc::new(AtomicBool::new(false));
        let state = poisoned.clone();
        #[cfg(feature = "local")]
        let capacity = Arc::new(tokio::sync::Notify::new());
        #[cfg(feature = "local")]
        let capacity_thread = capacity.clone();
        let result = std::thread::Builder::new()
            .name("debugbundle-callback".into())
            .spawn(move || {
                let _slot = slot;
                INTERNAL.with(|flag| flag.set(true));
                while let Ok(job) = rx.recv() {
                    #[cfg(feature = "local")]
                    capacity_thread.notify_waiters();
                    job();
                    if state.load(Ordering::Acquire) {
                        break;
                    }
                }
                // Retain both process permits through queued closure disposal,
                // including jobs racing a poisoned worker's final receive.
                for job in rx.try_iter() {
                    #[cfg(feature = "local")]
                    capacity_thread.notify_waiters();
                    dispose(job);
                }
                dispose(rx);
                #[cfg(feature = "local")]
                capacity_thread.notify_waiters();
                CALLBACKS.fetch_sub(1, Ordering::AcqRel);
            });
        if result.is_err() {
            CALLBACKS.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Self {
            sender: tx,
            poisoned,
            #[cfg(feature = "local")]
            capacity,
        })
    }
    #[cfg(feature = "local")]
    pub(crate) fn native_cleanup(&self) -> NativeCleanup {
        NativeCleanup {
            sender: self.sender.clone(),
            capacity: self.capacity.clone(),
        }
    }
    pub(crate) async fn call<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, CallbackFailure> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(CallbackFailure::Unavailable);
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let poisoned = self.poisoned.clone();
        let job = Box::new(move || {
            CALLBACK.with(|flag| flag.set(true));
            let _guard = CallbackGuard;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
            let value = match result {
                Ok(value) => Ok(value),
                Err(payload) => {
                    if !payload.is::<String>() && !payload.is::<&'static str>() {
                        poisoned.store(true, Ordering::Release);
                    }
                    discard_panic(payload);
                    Err(CallbackFailure::Panicked)
                }
            };
            let _ = tx.send(value);
        });
        self.sender
            .try_send(job)
            .map_err(|_| CallbackFailure::Unavailable)?;
        rx.await.unwrap_or(Err(CallbackFailure::Unavailable))
    }
}
fn discard_panic(mut payload: Box<dyn std::any::Any + Send>) {
    // Worker callbacks/cleanup keep disposal on their already-bounded worker;
    // rejected configuration stays in the explicit synchronous bootstrap call.
    // Repeated destructor panics occupy that same thread, never a replacement,
    // and no opaque payload is retained in a telemetry queue.
    loop {
        payload = match payload.downcast::<serde_json::Value>() {
            Ok(value) => {
                crate::owned_json::discard(*value);
                return;
            }
            Err(payload) => payload,
        };
        payload = match payload.downcast::<Vec<serde_json::Value>>() {
            Ok(values) => {
                crate::owned_json::discard(serde_json::Value::Array(*values));
                return;
            }
            Err(payload) => payload,
        };
        payload = match payload.downcast::<serde_json::Map<String, serde_json::Value>>() {
            Ok(values) => {
                crate::owned_json::discard(serde_json::Value::Object(*values));
                return;
            }
            Err(payload) => payload,
        };
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(payload))) {
            Ok(()) => return,
            Err(next) => payload = next,
        }
    }
}
pub(crate) fn spawn(future: impl Future<Output = ()> + Send + 'static) -> bool {
    if let Some(handle) = handle() {
        handle.spawn(future);
        true
    } else {
        false
    }
}
