//! Explicit, revocable global dispatch. No panic payload leaves its native hook.
use super::{capture, Client, Inner};
use serde_json::{json, Map, Value};
use std::{
    panic::PanicHookInfo,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, Weak,
    },
};
static REGISTRATIONS: Mutex<Vec<Weak<Registration>>> = Mutex::new(Vec::new());
static INSTALLED: AtomicBool = AtomicBool::new(false);
thread_local! {static OBSERVING:std::cell::Cell<bool>=const{std::cell::Cell::new(false)};}
pub(super) fn observing() -> bool {
    OBSERVING.with(std::cell::Cell::get)
}
struct Entered;
impl Drop for Entered {
    fn drop(&mut self) {
        OBSERVING.with(|flag| flag.set(false));
    }
}
fn enter() -> Option<Entered> {
    OBSERVING.with(|flag| {
        // A rejected nested hook must not acquire and drop the outer hook's guard.
        if flag.replace(true) {
            None
        } else {
            Some(Entered)
        }
    })
}
struct Registration {
    active: AtomicBool,
    client: Weak<Inner>,
}
impl Registration {
    fn live(&self) -> bool {
        self.active.load(Ordering::Acquire)
            && self
                .client
                .upgrade()
                .is_some_and(|client| !client.stopped.load(Ordering::Acquire))
    }
}
#[derive(Default)]
pub struct PanicObserver {
    registration: Option<Arc<Registration>>,
}
impl PanicObserver {
    pub fn is_active(&self) -> bool {
        self.registration
            .as_ref()
            .is_some_and(|registration| registration.live())
    }
}
impl Drop for PanicObserver {
    fn drop(&mut self) {
        if let Some(registration) = &self.registration {
            registration.active.store(false, Ordering::Release);
            if let Some(client) = registration.client.upgrade() {
                client.panic_observer.store(false, Ordering::Release);
            }
        }
    }
}
impl Client {
    /// Install once during application bootstrap. Detach revokes this client;
    /// it never restores a previous hook over one installed later by the app.
    pub fn install_panic_observer(&self) -> PanicObserver {
        if std::thread::panicking() {
            return PanicObserver::default();
        }
        let Some(inner) = self
            .inner
            .as_ref()
            .filter(|inner| !inner.stopped.load(Ordering::Acquire))
        else {
            return PanicObserver::default();
        };
        let Ok(mut registrations) = REGISTRATIONS.try_lock() else {
            return PanicObserver::default();
        };
        registrations.retain(|registration| {
            registration
                .upgrade()
                .is_some_and(|registration| registration.live())
        });
        if registrations.len() >= 8
            || registrations.try_reserve(1).is_err()
            || inner
                .panic_observer
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return PanicObserver::default();
        }
        if !INSTALLED.swap(true, Ordering::AcqRel) {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                let guard = enter();
                if guard.is_some() && !crate::runtime::internal() {
                    observe(info);
                }
                previous(info);
            }));
        }
        let registration = Arc::new(Registration {
            active: AtomicBool::new(true),
            client: Arc::downgrade(inner),
        });
        registrations.push(Arc::downgrade(&registration));
        PanicObserver {
            registration: Some(registration),
        }
    }
}
fn observe(info: &PanicHookInfo<'_>) {
    let registrations = if let Ok(registrations) = REGISTRATIONS.try_lock() {
        registrations
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>()
    } else {
        return;
    };
    for registration in registrations {
        if !registration.active.load(Ordering::Acquire) {
            continue;
        }
        let Some(inner) = registration
            .client
            .upgrade()
            .filter(|inner| !inner.stopped.load(Ordering::Acquire))
        else {
            continue;
        };
        let client = Client::retained(inner);
        client.enqueue_with_receipt(
            4,
            |_| true,
            |inner, _state| snapshot(inner, info),
            capture::CaptureMode::Panic,
            |_, _| {},
        );
    }
}
fn snapshot(inner: &Inner, info: &PanicHookInfo<'_>) -> Option<Value> {
    if inner.config.sample_rate == 0.0 {
        return None;
    }
    // Downcasts inspect only native Any identity. Custom payload formatters,
    // destructors, source accessors and application context are never traversed.
    let text = info
        .payload()
        .downcast_ref::<&'static str>()
        .copied()
        .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str));
    let message = crate::model::text(
        &inner
            .privacy
            .text(text.unwrap_or("Rust panic payload unavailable")),
        1024,
    );
    let snapshot = crate::ExceptionSnapshot::new("RustPanic", &message);
    let mut payload = json!({"name":snapshot.name(),"message":snapshot.message(),"stack":"Stack unavailable","handled":false,"request":{"method":"UNKNOWN","path":"/","query":{},"headers":{}},"response":{"status_code":0},"runtime":{"version":option_env!("DEBUGBUNDLE_RUSTC_VERSION").unwrap_or("unknown"),"platform":std::env::consts::OS,"arch":std::env::consts::ARCH,"pid":std::process::id()}});
    if inner.config.capture_exception_evidence {
        payload["exception_evidence"] = snapshot.evidence().clone();
    }
    let mut correlation = Map::new();
    if let Some(context) = crate::operations::current() {
        if let Some(operation) = context
            .operation
            .as_ref()
            .filter(|operation| operation.matches(&inner.operations))
        {
            if let Some(execution) = operation.execution(&inner.operations) {
                payload["execution"] = execution;
            }
            if let Some(trace) = context
                .correlation
                .get("trace_id")
                .and_then(Value::as_str)
                .and_then(crate::operations::trace_id)
            {
                correlation.insert("trace_id".into(), json!(trace));
            }
        }
    }
    let additions = json!({"exception_evidence":payload.get("exception_evidence"),"execution":payload.get("execution")});
    if crate::wire::canonical_size(&additions).is_none_or(|bytes| bytes > 4096) {
        if let Some(payload) = payload.as_object_mut() {
            payload.remove("execution");
            payload.remove("exception_evidence");
        }
    }
    let location=info.location().map(|location|json!({"file":crate::model::text(&inner.privacy.text(location.file()),1024),"line":location.line(),"column":location.column()}));
    capture::envelope(
        inner,
        "backend_exception",
        payload,
        json!({"panic":{"payload_available":text.is_some(),"location":location}}),
        &correlation,
    )
}

#[cfg(test)]
mod tests {
    use super::{enter, observing};

    #[test]
    fn rejected_nested_observation_preserves_the_outer_guard() {
        let outer = enter().expect("outer panic observation enters");
        for _ in 0..2 {
            assert!(enter().is_none());
            assert!(observing(), "rejection must retain outer panic suppression");
        }
        drop(outer);
        assert!(!observing());
        assert!(enter().is_some());
        assert!(!observing());
    }
}
