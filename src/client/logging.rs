use super::{capture, Client};
use crate::{logging::NativeLog, Level};
use serde_json::json;
use std::sync::atomic::Ordering;
impl Client {
    #[cfg(feature = "tracing")]
    pub(crate) fn record_logging_span(
        &self,
        layer: &str,
        id: u64,
        parent: Option<u64>,
        value: &serde_json::Value,
    ) {
        if let Some(inner) = &self.inner {
            if !inner.logging_enabled.load(Ordering::Acquire) {
                return;
            }
            if let Some(value) = inner.privacy.protect(value) {
                if let Some(mut registry) = inner.span_registry() {
                    registry.insert(layer, id, parent, value);
                }
            }
        }
    }
    #[cfg(feature = "tracing")]
    pub(crate) fn update_logging_span(&self, layer: &str, id: u64, fields: &serde_json::Value) {
        if let Some(inner) = &self.inner {
            if !inner.logging_enabled.load(Ordering::Acquire) {
                return;
            }
            if let Some(fields) = inner.privacy.protect(fields) {
                if let Some(mut registry) = inner.span_registry() {
                    registry.update(layer, id, &fields);
                }
            }
        }
    }
    #[cfg(feature = "tracing")]
    pub(crate) fn snapshot_logging_spans(&self, layer: &str, id: Option<u64>) -> serde_json::Value {
        self.inner
            .as_ref()
            .and_then(|inner| inner.span_registry())
            .map(|registry| registry.snapshot(layer, id))
            .unwrap_or_else(|| json!([]))
    }
    #[cfg(feature = "tracing")]
    pub(crate) fn close_logging_span(&self, layer: &str, id: u64) {
        if let Some(inner) = &self.inner {
            if let Some(mut registry) = inner.span_registry() {
                registry.close(layer, id);
            } else {
                inner.logging_spans_invalid.store(true, Ordering::Release);
            }
        }
    }
    #[cfg(feature = "tracing")]
    pub(crate) fn detach_logging_spans(&self, layer: &str) {
        if let Some(inner) = &self.inner {
            if let Some(mut registry) = inner.span_registry() {
                registry.detach(layer);
            } else {
                inner.logging_spans_invalid.store(true, Ordering::Release);
            }
        }
    }
    pub fn detach_logging(&self) {
        if let Some(inner) = &self.inner {
            inner.logging_enabled.store(false, Ordering::Release);
            #[cfg(feature = "tracing")]
            inner.logging_spans_invalid.store(true, Ordering::Release);
        }
    }
    pub(crate) fn native_logging_enabled(&self, level: Level) -> bool {
        if crate::runtime::internal() || !self.log_enabled(level) {
            return false;
        }
        self.inner.as_ref().is_some_and(|inner| {
            inner.logging_enabled.load(Ordering::Acquire)
                && inner.state.try_lock().ok().is_some_and(|state| {
                    state.owned < inner.config.max_buffered_events
                        && state.bytes < inner.config.max_buffered_bytes.saturating_sub(1024)
                })
        })
    }
    pub(crate) fn capture_native_log(&self, record: &NativeLog) {
        if !self.native_logging_enabled(record.level) {
            return;
        }
        let Some(inner) = &self.inner else {
            return;
        };
        let association = crate::occurrence::current(&inner.operations);
        let exception =
            record.native_error || (record.level >= Level::Error && association.is_some());
        let mut metadata = record.metadata.clone();
        if exception {
            metadata["level"] = json!(record.level.as_str());
            metadata["message"] = json!(record.message);
            metadata["attributes"] = record.attributes.clone();
        }
        let context = association
            .as_ref()
            .map(|association| association.context.clone())
            .unwrap_or_default()
            .with("logging", &metadata);
        let owner = capture::effective(inner, &context)
            .operation
            .filter(|operation| operation.matches(&inner.operations))
            .and_then(|operation| operation.operation_id().map(str::to_owned));
        let pair = exception
            && association.is_some()
            && owner.is_some()
            && inner.config.before_send.is_none()
            && inner.config.sample_rate == 1.0;
        self.enqueue_with_receipt(if exception{4}else if record.level>=Level::Error{3}else{1},|state|{
            state.policy.log(record.level,inner.config.log_level)&&!(pair&&association.as_ref().zip(owner.as_deref()).is_some_and(|(association,owner)|state.occurrences.contains(association.occurrence.id(),owner,std::time::Instant::now())))
        },|inner,state|{
            if exception{
                if let Some(association)=&association{capture::exception(inner,state,association.occurrence.snapshot(),&context)}else{
                    let mut snapshot=crate::ExceptionSnapshot::new("RustError","Error details unavailable without a safe snapshot");
                    snapshot.mark_unsafe();capture::exception(inner,state,&snapshot,&context)
                }
            }else{capture::base(inner,state,"log_event",json!({"level":record.level.as_str(),"message":record.message,"attributes":record.attributes}),&context)}
        },if pair{capture::CaptureMode::PairedLog}else{capture::CaptureMode::Ordinary},|state,event|{
            if pair{if let Some((association,owner))=association.as_ref().zip(owner.as_deref()){state.occurrences.record(association.occurrence.id(),owner,event["event_id"].as_str().unwrap_or(""),std::time::Instant::now(),inner.config.max_event_age);}}
        });
    }
}
#[cfg(feature = "tracing")]
impl super::Inner {
    fn span_registry(&self) -> Option<std::sync::MutexGuard<'_, crate::logging::spans::Registry>> {
        let mut registry = self.logging_spans.try_lock().ok()?;
        // A contended close cannot leave dead span state occupying the cache.
        // Conservative invalidation drops optional metadata without waiting on a
        // request or attempting to inspect the application's subscriber state.
        if self.logging_spans_invalid.swap(false, Ordering::AcqRel) {
            registry.clear();
        }
        Some(registry)
    }
}
