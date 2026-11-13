//! Shared client state and finite administrative lifecycle operations.
mod capture;
#[cfg(feature = "http")]
mod configuration;
mod correlation;
#[cfg(any(feature = "tower", feature = "actix-web"))]
mod http;
#[cfg(any(feature = "tracing", feature = "log"))]
mod logging;
mod occurrence;
mod panic;
#[cfg(feature = "local")]
mod relay;
#[cfg(feature = "local")]
mod spool;
pub use panic::PanicObserver;
mod probes;
mod recovery;
mod response;
mod sender;
#[cfg(feature = "testkit")]
mod testkit;
use crate::{
    config::{Config, ProjectMode},
    policy::Policy,
    privacy::Privacy,
    runtime::{self, CallbackWorker},
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::Notify;

pub struct Client {
    inner: Option<Arc<Inner>>,
    reason: Option<&'static str>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    Healthy,
    Degraded,
    Disconnected,
}
pub struct Status {
    pub state: ConnectionStatus,
    pub reason: Option<&'static str>,
    pub buffered_events: usize,
    pub buffered_bytes: usize,
    pub dropped_events: u64,
}
pub struct FlushOutcome {
    pub drained: bool,
    pub timed_out: bool,
    pub accepted_events: u64,
    pub dropped_events: u64,
}

struct Entry {
    event: Value,
    bytes: usize,
    priority: u8,
    admitted: Instant,
    finalized: bool,
    hook_allowed: bool,
    receipt: Option<String>,
}
struct State {
    occurrences: crate::occurrence::Registry,
    diagnostics: crate::diagnostics::Ledger,
    probes: crate::probes::Rings,
    remote: crate::probes::Remote,
    #[cfg(feature = "http")]
    config_etag: Option<String>,
    pending_probe: Option<String>,
    suppression: crate::suppression::Suppression,
    pending: VecDeque<Entry>,
    owned: usize,
    bytes: usize,
    policy: Policy,
    features: Vec<String>,
    context: Value,
    next_attempt: Instant,
    failures: u32,
    rate_limited: bool,
    flush_deadline: Option<Instant>,
}
struct Inner {
    #[cfg(feature = "local")]
    spool_started: AtomicBool,
    #[cfg(feature = "local")]
    spool_revision: AtomicU64,
    #[cfg(feature = "local")]
    spool_admitting: AtomicUsize,
    #[cfg(feature = "local")]
    spool_lease: Arc<crate::relay::spool::LeaseReservation>,
    #[cfg(feature = "local")]
    spool_wake: Notify,
    #[cfg(feature = "local")]
    spool_status: Mutex<crate::relay::SpoolStatus>,
    #[cfg(feature = "local")]
    relay_budget: Arc<crate::relay::IngressBudget>,
    panic_observer: AtomicBool,
    slot: Arc<runtime::ClientSlot>,
    #[cfg(feature = "tracing")]
    logging_spans: Mutex<crate::logging::spans::Registry>,
    #[cfg(feature = "tracing")]
    logging_spans_invalid: AtomicBool,
    operations: Arc<crate::operations::OperationBudget>,
    config: Config,
    privacy: Privacy,
    state: Mutex<State>,
    callbacks: CallbackWorker,
    wake: Notify,
    changed: Notify,
    stop: Notify,
    #[cfg(feature = "http")]
    config_inflight: AtomicBool,
    stopped: AtomicBool,
    handles: AtomicUsize,
    dropped: AtomicU64,
    accepted: AtomicU64,
    last_event: AtomicU64,
    pressure: AtomicU64,
    pressure_first: AtomicU64,
    pressure_last: AtomicU64,
    log_level: AtomicU8,
    #[cfg(any(feature = "tracing", feature = "log"))]
    logging_enabled: AtomicBool,
    #[cfg(feature = "http")]
    http: Option<crate::transport::HttpTransport>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        runtime::defer_configuration(std::mem::take(&mut self.config), self.slot.clone());
    }
}
impl Clone for Client {
    fn clone(&self) -> Self {
        if let Some(inner) = &self.inner {
            inner.handles.fetch_add(1, Ordering::Relaxed);
        }
        Self {
            inner: self.inner.clone(),
            reason: self.reason,
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        if let Some(inner) = &self.inner {
            if inner.handles.fetch_sub(1, Ordering::AcqRel) == 1 {
                inner.stopped.store(true, Ordering::Release);
                inner.stop.notify_waiters();
                inner.operations.enabled.store(false, Ordering::Release);
                inner.wake.notify_one();
                inner.changed.notify_waiters();
            }
        }
    }
}

impl Client {
    fn retained(inner: Arc<Inner>) -> Self {
        inner.handles.fetch_add(1, Ordering::AcqRel);
        Self {
            inner: Some(inner),
            reason: None,
        }
    }
    pub fn start_operation(&self, options: crate::OperationOptions) -> crate::Operation {
        self.inner
            .as_ref()
            .filter(|inner| !inner.stopped.load(Ordering::Acquire))
            .map(|inner| {
                crate::operations::start(inner.operations.clone(), &inner.privacy, options)
            })
            .unwrap_or_default()
    }
    pub fn new(mut config: Config) -> Self {
        let inactive = |reason| Self {
            inner: None,
            reason: Some(reason),
        };
        let proof_valid = if let Some(raw) = config.evidence_capabilities.take() {
            let descriptor = crate::wire::descriptor(&crate::wire::capabilities(&raw));
            let valid = crate::wire::valid_descriptor(&raw);
            crate::owned_json::discard(raw);
            config.evidence_capabilities = valid.then_some(descriptor);
            valid
        } else {
            true
        };
        if !config.enabled {
            return inactive("disabled");
        }
        if !proof_valid || config.validate().is_err() {
            return inactive("invalid_configuration");
        }
        let Some(privacy) = Privacy::new(&config.redact_fields) else {
            return inactive("invalid_privacy");
        };
        if !crate::wire::prepare() || runtime::handle().is_none() {
            return inactive("runtime_unavailable");
        }
        if !runtime::claim(&runtime::CLIENTS) {
            return inactive("client_limit");
        }
        let slot = Arc::new(runtime::ClientSlot);
        let Some(callbacks) = CallbackWorker::new(slot.clone()) else {
            return inactive("callback_limit");
        };
        #[cfg(feature = "http")]
        let http = if config.project_mode == ProjectMode::Connected && config.transport.is_none() {
            crate::transport::HttpTransport::new(
                &config.endpoint,
                &config.project_token,
                config.request_timeout,
                config.allow_insecure_http,
                config.proxy.as_deref(),
            )
        } else {
            None
        };
        #[cfg(feature = "http")]
        if config.project_mode == ProjectMode::Connected
            && config.transport.is_none()
            && http.is_none()
        {
            return inactive("transport_unavailable");
        }
        let features = config
            .evidence_capabilities
            .as_ref()
            .map(crate::wire::capabilities)
            .unwrap_or_default();
        let log_level = config.log_level.max(crate::Level::Warning) as u8;
        // Explicit custom/local readers use the established legacy policy. A
        // native authenticated endpoint starts minimal until discovery succeeds.
        let initial_policy =
            if config.transport.is_some() || config.project_mode == ProjectMode::LocalOnly {
                Policy::legacy()
            } else {
                Policy::minimal()
            };
        let inner = Arc::new(Inner {
            #[cfg(feature = "local")]
            spool_started: AtomicBool::new(false),
            #[cfg(feature = "local")]
            spool_revision: AtomicU64::new(0),
            #[cfg(feature = "local")]
            spool_admitting: AtomicUsize::new(0),
            #[cfg(feature = "local")]
            spool_lease: Arc::new(crate::relay::spool::LeaseReservation::default()),
            #[cfg(feature = "local")]
            spool_wake: Notify::new(),
            #[cfg(feature = "local")]
            spool_status: Mutex::new(crate::relay::SpoolStatus::default()),
            #[cfg(feature = "local")]
            relay_budget: Arc::new(crate::relay::IngressBudget::default()),
            panic_observer: AtomicBool::new(false),
            slot,
            #[cfg(feature = "tracing")]
            logging_spans: Mutex::new(crate::logging::spans::Registry::default()),
            #[cfg(feature = "tracing")]
            logging_spans_invalid: AtomicBool::new(false),
            operations: Arc::new(crate::operations::OperationBudget::default()),
            config,
            privacy,
            state: Mutex::new(State {
                occurrences: crate::occurrence::Registry::default(),
                diagnostics: crate::diagnostics::Ledger::default(),
                probes: crate::probes::Rings::default(),
                remote: crate::probes::Remote::default(),
                #[cfg(feature = "http")]
                config_etag: None,
                pending_probe: None,
                suppression: crate::suppression::Suppression::default(),
                pending: VecDeque::new(),
                owned: 0,
                bytes: 0,
                policy: initial_policy,
                features,
                context: json!({}),
                next_attempt: Instant::now(),
                failures: 0,
                rate_limited: false,
                flush_deadline: None,
            }),
            callbacks,
            wake: Notify::new(),
            changed: Notify::new(),
            stop: Notify::new(),
            #[cfg(feature = "http")]
            config_inflight: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            handles: AtomicUsize::new(1),
            dropped: AtomicU64::new(0),
            accepted: AtomicU64::new(0),
            last_event: AtomicU64::new(0),
            pressure: AtomicU64::new(0),
            pressure_first: AtomicU64::new(0),
            pressure_last: AtomicU64::new(0),
            log_level: AtomicU8::new(log_level),
            #[cfg(any(feature = "tracing", feature = "log"))]
            logging_enabled: AtomicBool::new(true),
            #[cfg(feature = "http")]
            http,
        });
        runtime::spawn(sender::run(Arc::downgrade(&inner)));
        #[cfg(feature = "http")]
        if inner.http.is_some() {
            runtime::spawn(configuration::run(Arc::downgrade(&inner)));
        }
        Self {
            inner: Some(inner),
            reason: None,
        }
    }
    pub fn status(&self) -> Status {
        let Some(inner) = &self.inner else {
            return Status {
                state: ConnectionStatus::Disconnected,
                reason: self.reason,
                buffered_events: 0,
                buffered_bytes: 0,
                dropped_events: 0,
            };
        };
        let Some(state) = inner.state.lock().ok() else {
            return Status {
                state: ConnectionStatus::Disconnected,
                reason: Some("state_unavailable"),
                buffered_events: 0,
                buffered_bytes: 0,
                dropped_events: inner.dropped.load(Ordering::Relaxed),
            };
        };
        Status {
            state: if inner.stopped.load(Ordering::Acquire)
                || state.failures >= 3
                || (!inner.callbacks.available()
                    && (inner.config.before_send.is_some()
                        || inner.config.transport.is_some()
                        || inner.config.project_mode == ProjectMode::LocalOnly))
            {
                ConnectionStatus::Disconnected
            } else if state.rate_limited {
                ConnectionStatus::Degraded
            } else {
                ConnectionStatus::Healthy
            },
            reason: (!inner.callbacks.available()).then_some("callback_unavailable"),
            buffered_events: state.owned,
            buffered_bytes: state.bytes,
            dropped_events: inner.dropped.load(Ordering::Relaxed),
        }
    }
    pub fn last_event_at(&self) -> Option<u64> {
        self.inner
            .as_ref()
            .map(|i| i.last_event.load(Ordering::Relaxed))
            .filter(|n| *n != 0)
    }
    pub fn log_enabled(&self, level: crate::Level) -> bool {
        !runtime::internal()
            && !panic::observing()
            && self.inner.as_ref().is_some_and(|inner| {
                !inner.stopped.load(Ordering::Acquire)
                    && (level as u8) >= inner.log_level.load(Ordering::Acquire)
            })
    }
    pub fn remote_config_ready(&self) -> bool {
        self.inner
            .as_ref()
            .and_then(|inner| inner.state.lock().ok())
            .is_some_and(|state| state.remote.initialized)
    }
    pub fn set_context(&self, context: &crate::Context) {
        if let Some(inner) = &self.inner {
            if let Some(protected) = inner.privacy.protect(context.as_value()) {
                if let Ok(mut state) = inner.state.try_lock() {
                    state.context = protected;
                }
            }
        }
    }
    pub fn flush(&self, timeout: Duration) -> FlushOutcome {
        if crate::runtime::internal() {
            return self.outcome(false);
        }
        let Some(inner) = &self.inner else {
            return self.outcome(true);
        };
        let deadline = Instant::now()
            .checked_add(timeout.min(Duration::from_secs(30)))
            .unwrap_or_else(Instant::now);
        if let Ok(mut state) = inner.state.lock() {
            state.flush_deadline = Some(
                state
                    .flush_deadline
                    .filter(|d| *d > Instant::now())
                    .map_or(deadline, |old| old.min(deadline)),
            );
        }
        inner.wake.notify_one();
        loop {
            let drained = inner.state.lock().ok().is_none_or(|state| {
                state.owned == 0
                    && state.pending_probe.is_none()
                    && inner.pressure.load(Ordering::Acquire) == 0
            });
            if drained {
                return self.outcome(true);
            }
            if Instant::now() >= deadline {
                return self.outcome(false);
            }
            // Administrative synchronous flush never drives the app runtime;
            // sender/callback/config work runs on the SDK-owned runtime.
            std::thread::sleep(
                Duration::from_millis(1).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
    pub async fn flush_async(&self, timeout: Duration) -> FlushOutcome {
        if crate::runtime::internal() {
            return self.outcome(false);
        }
        let Some(inner) = &self.inner else {
            return self.outcome(true);
        };
        let deadline = tokio::time::Instant::now() + timeout.min(Duration::from_secs(30));
        if let Ok(mut state) = inner.state.lock() {
            let requested = deadline.into_std();
            state.flush_deadline = Some(
                state
                    .flush_deadline
                    .filter(|d| *d > Instant::now())
                    .map_or(requested, |old| old.min(requested)),
            );
        }
        inner.wake.notify_one();
        loop {
            let notified = inner.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if inner.state.lock().ok().is_none_or(|state| {
                state.owned == 0
                    && state.pending_probe.is_none()
                    && inner.pressure.load(Ordering::Acquire) == 0
            }) {
                return self.outcome(true);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return self.outcome(false);
            }
        }
    }
    pub fn shutdown(&self, timeout: Duration) -> FlushOutcome {
        let result = self.flush(timeout);
        if let Some(inner) = &self.inner {
            inner.stopped.store(true, Ordering::Release);
            inner.stop.notify_waiters();
            inner.operations.enabled.store(false, Ordering::Release);
            inner.wake.notify_one();
        }
        result
    }
    pub async fn shutdown_async(&self, timeout: Duration) -> FlushOutcome {
        let result = self.flush_async(timeout).await;
        if let Some(inner) = &self.inner {
            inner.stopped.store(true, Ordering::Release);
            inner.stop.notify_waiters();
            inner.operations.enabled.store(false, Ordering::Release);
            inner.wake.notify_one();
        }
        result
    }
    fn outcome(&self, drained: bool) -> FlushOutcome {
        FlushOutcome {
            drained,
            timed_out: !drained,
            accepted_events: self
                .inner
                .as_ref()
                .map_or(0, |i| i.accepted.load(Ordering::Relaxed)),
            dropped_events: self
                .inner
                .as_ref()
                .map_or(0, |i| i.dropped.load(Ordering::Relaxed)),
        }
    }
}
pub(crate) fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
impl Inner {
    fn record_pressure(&self) {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        let _ =
            self.pressure_first
                .compare_exchange(0, millis, Ordering::AcqRel, Ordering::Acquire);
        self.pressure_last.store(millis, Ordering::Release);
        self.pressure.fetch_add(1, Ordering::AcqRel);
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }
}
