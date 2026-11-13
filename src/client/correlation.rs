use super::{capture, Client};
use crate::Context;
use std::sync::atomic::Ordering;
impl Client {
    /// Return the product trace header value for an explicitly allowed origin.
    /// Re-evaluate at every redirect; the caller owns its HTTP client and headers.
    pub fn trace_header_for(&self, destination: &str, context: &Context) -> Option<String> {
        let inner = self.inner.as_ref()?;
        if inner.stopped.load(Ordering::Acquire) || destination.len() > 8192 {
            return None;
        }
        let url = url::Url::parse(destination).ok()?;
        if !matches!(url.scheme(), "https" | "http")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return None;
        }
        let origin = url.origin().ascii_serialization();
        // The entire ingestion origin is reserved, including prefixed config paths.
        if url::Url::parse(&inner.config.endpoint)
            .ok()
            .is_some_and(|endpoint| endpoint.origin() == url.origin())
        {
            return None;
        }
        if !inner
            .config
            .trace_propagation_targets
            .iter()
            .any(|target| crate::config::trace_origin(target).as_deref() == Some(&origin))
        {
            return None;
        }
        let context = capture::effective(inner, context);
        context
            .correlation
            .get("trace_id")
            .and_then(serde_json::Value::as_str)
            .and_then(crate::operations::trace_id)
    }
}
