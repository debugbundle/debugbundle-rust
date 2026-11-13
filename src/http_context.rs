//! Shared owned HTTP metadata; native streams and native HTTP types stay in adapters.
use crate::{Client, Context, Operation, RequestSnapshot};
use serde_json::Value;
use std::{
    sync::{
        atomic::{AtomicU16, Ordering},
        Arc,
    },
    time::Instant,
};

/// An explicit already-parsed primitive snapshot. Middleware never reads a body.
#[derive(Clone, Debug)]
pub struct RequestBodySnapshot(pub(crate) Value);
impl RequestBodySnapshot {
    pub fn new(value: &Value) -> Option<Self> {
        // Check the source as well as its protected result: truncation is not opt-in.
        if !crate::wire::preflight(value) || crate::wire::canonical_size(value)? > 8192 {
            return None;
        }
        let value = crate::model::protected(value)?;
        (crate::wire::canonical_size(&value)? <= 8192).then_some(Self(value))
    }
}

pub(crate) fn bounded_target(value: &str) -> String {
    if value.len() > 16384 {
        "/[withheld]".into()
    } else {
        value.to_owned()
    }
}

#[derive(Clone, Debug, Default)]
pub struct HttpOptions {
    /// Supply the actual resolved framework version, for example from build metadata.
    pub framework_version: Option<String>,
    /// Exact additional paths excluded from request instrumentation.
    pub excluded_paths: Vec<String>,
}
impl HttpOptions {
    pub(crate) fn normalized(mut self) -> Self {
        self.framework_version = self.framework_version.filter(|version| {
            !version.is_empty()
                && version.len() <= 64
                && version
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-+_".contains(&b))
        });
        self.excluded_paths.truncate(64);
        self.excluded_paths.retain(|path| {
            path.len() <= 2048 && path.starts_with('/') && !path.contains(['?', '#'])
        });
        self
    }
}

#[derive(Debug)]
pub(crate) struct RequestData {
    pub snapshot: RequestSnapshot,
    pub framework: &'static str,
    pub version: Option<String>,
    pub status: AtomicU16,
    pub all: bool,
    pub client_errors: [bool; 100],
}
impl RequestData {
    pub fn retained_bytes(&self) -> usize {
        self.snapshot.method.len()
            + self.snapshot.path.len()
            + self.snapshot.route_template.as_ref().map_or(0, String::len)
            + crate::wire::canonical_size(&self.snapshot.headers).unwrap_or(262144)
            + self
                .snapshot
                .body
                .as_ref()
                .and_then(crate::wire::canonical_size)
                .unwrap_or(0)
            + self.version.as_ref().map_or(0, String::len)
            + 256
    }
    pub fn selected(&self, status: u16) -> bool {
        self.all
            || status >= 500
            || (400..500).contains(&status) && self.client_errors[usize::from(status - 400)]
    }
}

pub(crate) struct RequestLifecycle {
    client: Client,
    pub owner: Operation,
    pub data: Arc<RequestData>,
    started: Instant,
}
impl RequestLifecycle {
    pub(crate) fn new(client: Client, owner: Operation, data: Arc<RequestData>) -> Self {
        Self {
            client,
            owner,
            data,
            started: Instant::now(),
        }
    }
    pub(crate) fn context(&self) -> Context {
        self.owner.context()
    }
    pub(crate) fn headers(&self, status: u16, route: Option<&str>) {
        self.data.status.store(status, Ordering::Release);
        if !self.data.selected(status) {
            return;
        }
        let mut snapshot = self.data.snapshot.clone();
        snapshot.status = status;
        snapshot.duration_ms = self.started.elapsed().as_secs_f64() * 1000.0;
        if let Some(route) = route.filter(|route| route.len() <= 2048) {
            snapshot.route_template = Some(crate::model::text(route, 2048));
        }
        self.client.capture_request(&snapshot, &self.context());
    }
    pub(crate) fn error<E: ?Sized>(&self, _: &E) {
        let mut snapshot = crate::ExceptionSnapshot::new(
            std::any::type_name::<E>(),
            "Error details unavailable without a safe snapshot",
        );
        snapshot.mark_unsafe();
        self.client.capture_exception(&snapshot, &self.context());
    }
    pub(crate) fn trace(&self) -> Option<String> {
        self.context()
            .correlation
            .get("trace_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
}
