#![cfg(any(feature = "tracing", feature = "log"))]
mod helpers;
use debugbundle::{Client, Context, Level};
use helpers::{configured, Recorder};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
static FORMATTERS: AtomicUsize = AtomicUsize::new(0);
struct UnsafeValue;
impl std::fmt::Debug for UnsafeValue {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        FORMATTERS.fetch_add(1, Ordering::SeqCst);
        panic!("must not format")
    }
}
impl std::fmt::Display for UnsafeValue {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        FORMATTERS.fetch_add(1, Ordering::SeqCst);
        panic!("must not display")
    }
}
impl std::error::Error for UnsafeValue {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        FORMATTERS.fetch_add(1, Ordering::SeqCst);
        panic!("must not inspect")
    }
}

#[cfg(feature = "tracing")]
#[test]
fn tracing_filters_only_its_layer_and_never_formats_opaque_fields() {
    use tracing_subscriber::{
        layer::{Context as LayerContext, Layer, SubscriberExt},
        Registry,
    };
    struct Other(Arc<AtomicUsize>);
    impl<S: tracing::Subscriber> Layer<S> for Other {
        fn on_event(&self, _: &tracing::Event<'_>, _: LayerContext<'_, S>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    let other = Arc::new(AtomicUsize::new(0));
    let subscriber = Registry::default()
        .with(Other(other.clone()))
        .with(client.tracing_layer());
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(message="info remains in the app sink",unsafe_value=?UnsafeValue);
        tracing::warn!(message="password=SYNTHETIC_SECRET",technical_count=3u64,opaque=?UnsafeValue);
        tracing::error!(
            message = "native error",
            error = &UnsafeValue as &(dyn std::error::Error + 'static)
        );
    });
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert_eq!(FORMATTERS.load(Ordering::SeqCst), 0);
    assert_eq!(other.load(Ordering::SeqCst), 3);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| event["event_type"] == "backend_exception")
            .count(),
        1
    );
    assert_eq!(events[0]["payload"]["attributes"]["technical_count"], 3);
    assert!(!serde_json::to_string(&*events)
        .unwrap()
        .contains("SYNTHETIC_SECRET"));
}

#[cfg(feature = "log")]
#[test]
fn log_tee_preserves_existing_sink_and_dynamic_arguments_are_not_evaluated() {
    use log::Log;
    struct Other(Arc<AtomicUsize>);
    impl Log for Other {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, _: &log::Record<'_>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        fn flush(&self) {}
    }
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    let other = Arc::new(AtomicUsize::new(0));
    let logger = client.logger().tee(Arc::new(Other(other.clone())));
    logger.log(
        &log::Record::builder()
            .level(log::Level::Info)
            .args(format_args!("info"))
            .build(),
    );
    logger.log(
        &log::Record::builder()
            .level(log::Level::Error)
            .args(format_args!("literal"))
            .build(),
    );
    logger.log(
        &log::Record::builder()
            .level(log::Level::Error)
            .args(format_args!("{UnsafeValue}"))
            .build(),
    );
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert_eq!(FORMATTERS.load(Ordering::SeqCst), 0);
    assert_eq!(other.load(Ordering::SeqCst), 3);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["payload"]["message"], "literal");
    assert_eq!(
        events[1]["payload"]["message"],
        "Log message unavailable without formatting"
    );
}

#[cfg(feature = "tracing")]
#[test]
fn detach_stops_only_sdk_logging_and_full_error_queues_skip_field_visits() {
    use tracing_subscriber::layer::SubscriberExt;
    let recorder = Arc::new(Recorder::default());
    let mut config = configured(recorder.clone());
    config.max_buffered_events = 1;
    let client = Client::new(config);
    client.capture_log(Level::Error, "primary", &Context::default());
    let subscriber = tracing_subscriber::registry().with(client.tracing_layer());
    tracing::subscriber::with_default(subscriber, || {
        tracing::error!(message="overloaded",opaque=?UnsafeValue);
        client.detach_logging();
        tracing::error!(message = "detached");
    });
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert_eq!(FORMATTERS.load(Ordering::SeqCst), 0);
    assert_eq!(
        recorder
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event["event_type"] == "log_event")
            .count(),
        1
    );
}

#[cfg(feature = "tracing")]
#[test]
fn span_metadata_is_protected_updated_and_released_on_close() {
    use tracing_subscriber::layer::SubscriberExt;
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    let subscriber = tracing_subscriber::registry().with(client.tracing_layer());
    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::warn_span!("checkout",stage="parsed",password="SYNTHETIC_SECRET",opaque=?UnsafeValue);
        span.record("stage", "complete");
        {
            let _entered = span.enter();
            tracing::error!(message = "inside");
        }
        drop(span);
        tracing::error!(message = "outside");
    });
    assert!(client.flush(Duration::from_secs(2)).drained);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0]["context"]["logging"]["spans"][0]["name"],
        "checkout"
    );
    assert_eq!(
        events[0]["context"]["logging"]["spans"][0]["fields"]["stage"],
        "complete"
    );
    assert_eq!(
        events[1]["context"]["logging"]["spans"],
        serde_json::json!([])
    );
    assert!(!serde_json::to_string(&*events)
        .unwrap()
        .contains("SYNTHETIC_SECRET"));
    assert_eq!(FORMATTERS.load(Ordering::SeqCst), 0);
}

#[cfg(feature = "tracing")]
#[test]
fn exact_occurrences_pair_only_after_admission_in_the_same_operation() {
    use tracing_subscriber::layer::SubscriberExt;
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    let subscriber = tracing_subscriber::registry().with(client.tracing_layer());
    let occurrence = debugbundle::ExceptionOccurrence::new(&debugbundle::ExceptionSnapshot::new(
        "CheckoutError",
        "failed",
    ))
    .unwrap();
    let owner = client.start_operation(Default::default());
    let context = owner.context();
    tracing::subscriber::with_default(subscriber, || {
        client.with_error_log(&occurrence, &context, || {
            tracing::error!(message = "checkout failed")
        })
    });
    client.capture_occurrence(&occurrence, &context);
    let independent = client.start_operation(Default::default());
    client.capture_occurrence(&occurrence, &independent.context());
    assert!(client.flush(Duration::from_secs(2)).drained);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert!(events
        .iter()
        .all(|event| event["event_type"] == "backend_exception"));
    assert!(events
        .iter()
        .all(|event| event["payload"]["name"] == "CheckoutError"));
}

#[cfg(feature = "tracing")]
#[test]
fn deferred_hooks_and_failed_logger_admission_cannot_suppress_the_only_capture() {
    use tracing_subscriber::layer::SubscriberExt;
    for failed in [false, true] {
        let recorder = Arc::new(Recorder::default());
        let mut config = configured(recorder.clone());
        let hooks = Arc::new(AtomicUsize::new(0));
        let observed = hooks.clone();
        if !failed {
            config.before_send = Some(Arc::new(move |event| {
                if observed.fetch_add(1, Ordering::SeqCst) == 0 {
                    None
                } else {
                    Some(event)
                }
            }));
        }
        let client = Client::new(config);
        let subscriber = tracing_subscriber::registry().with(client.tracing_layer());
        let occurrence = debugbundle::ExceptionOccurrence::new(
            &debugbundle::ExceptionSnapshot::new("Failure", "failed"),
        )
        .unwrap();
        let owner = client.start_operation(Default::default());
        let context = owner.context();
        if failed {
            client.detach_logging();
        }
        tracing::subscriber::with_default(subscriber, || {
            client.with_error_log(&occurrence, &context, || {
                tracing::error!(message = "failed")
            })
        });
        client.capture_occurrence(&occurrence, &context);
        assert!(client.flush(Duration::from_secs(2)).drained);
        assert_eq!(recorder.events.lock().unwrap().len(), 1);
    }
}

#[cfg(feature = "tracing")]
#[test]
fn tracing_rechecks_the_same_callsite_after_queue_recovery_and_detach() {
    use std::time::Instant;
    use tracing_subscriber::layer::SubscriberExt;
    let recorder = Arc::new(Recorder::default());
    let mut config = configured(recorder.clone());
    config.max_buffered_events = 1;
    let client = Client::new(config);
    let deadline = Instant::now() + Duration::from_secs(2);
    while client.status().buffered_events == 0 {
        client.capture_log(Level::Error, "fill", &Context::default());
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let subscriber = tracing_subscriber::registry().with(client.tracing_layer());
    tracing::subscriber::with_default(subscriber, || {
        let emit = || tracing::error!(message = "dynamic callsite");
        emit();
        assert!(client.flush(Duration::from_secs(2)).drained);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            // Draining releases event ownership; it does not reserve an idle
            // state mutex for the next best-effort nonblocking capture. Repeat
            // the same callsite until observed, still failing if it was cached
            // permanently disabled while the queue was full.
            emit();
            assert!(client.flush(Duration::from_secs(2)).drained);
            if recorder
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|event| event["payload"]["message"] == "dynamic callsite")
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "recovered callsite stayed disabled"
            );
            std::thread::yield_now();
        }
        client.detach_logging();
        emit();
        assert!(client.flush(Duration::from_secs(1)).drained);
    });
    let events = recorder.events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["payload"]["message"] == "dynamic callsite")
            .count(),
        1
    );
}
