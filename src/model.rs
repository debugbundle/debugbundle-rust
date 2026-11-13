//! Owned data boundaries. No automatic application formatting or traversal.
use crate::privacy::Privacy;
use serde_json::{json, Value};
use std::{
    error::Error,
    sync::{Arc, LazyLock},
};

static BASE_PRIVACY: LazyLock<Option<Privacy>> = LazyLock::new(|| Privacy::new(&[]));
pub(crate) fn protected(value: &Value) -> Option<Value> {
    BASE_PRIVACY.as_ref()?.protect(value)
}
pub(crate) fn text(value: &str, limit: usize) -> String {
    let mut result = BASE_PRIVACY
        .as_ref()
        .map(|p| p.text(value))
        .unwrap_or_else(|| "[REDACTED]".into());
    if result.len() > limit {
        let mut end = limit;
        while !result.is_char_boundary(end) {
            end -= 1;
        }
        result.truncate(end);
    }
    result
}

#[derive(Clone, Debug)]
pub struct Context {
    data: Arc<Value>,
    pub(crate) operation: Option<crate::operations::OperationContext>,
    pub(crate) correlation: serde_json::Map<String, Value>,
    #[cfg(any(feature = "tower", feature = "actix-web"))]
    pub(crate) http: Option<Arc<crate::http_context::RequestData>>,
}
impl Default for Context {
    fn default() -> Self {
        Self {
            data: Arc::new(json!({})),
            operation: None,
            correlation: serde_json::Map::new(),
            #[cfg(any(feature = "tower", feature = "actix-web"))]
            http: None,
        }
    }
}
impl Context {
    pub fn new(value: &Value) -> Self {
        Self {
            data: Arc::new(
                protected(value)
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({})),
            ),
            ..Self::default()
        }
    }
    pub fn as_value(&self) -> &Value {
        &self.data
    }
    pub fn with(mut self, key: &str, value: &Value) -> Self {
        let mut data = (*self.data).clone();
        if let Some(value) = protected(value) {
            if let Some(map) = data.as_object_mut() {
                if key.len() <= 128 && map.len() < 256 {
                    map.insert(key.to_owned(), value);
                }
            }
        }
        self.data = Self::new(&data).data;
        self
    }
    pub fn with_trace_id(mut self, trace: &str) -> Self {
        if crate::operations::trace_id(trace).is_some() {
            self.correlation.insert("trace_id".into(), json!(trace));
        }
        self
    }
    pub fn with_request_id(mut self, id: &str) -> Self {
        if !id.is_empty()
            && id.len() <= 128
            && id.bytes().all(|byte| (33..=126).contains(&byte))
            && BASE_PRIVACY
                .as_ref()
                .is_some_and(|privacy| privacy.text(id) == id)
        {
            self.correlation.insert("request_id".into(), json!(id));
        }
        self
    }
    pub fn operation_active(&self) -> bool {
        self.operation
            .as_ref()
            .is_some_and(crate::operations::OperationContext::is_active)
    }
    pub fn with_session_id(mut self, id: &str) -> Self {
        if crate::operations::trace_id(id).is_some() {
            self.correlation.insert("session_id".into(), json!(id));
        }
        self
    }
    pub fn with_evidence_reference(self, failure_ref: &str, scope: &crate::EvidenceScope) -> Self {
        let Some(scope) = scope.value() else {
            return self;
        };
        let reference = json!({"version":1,"failure_ref":failure_ref,"scope":scope});
        if !crate::wire::valid_reference(&reference) {
            return self;
        }
        self.with("evidence_reference", &reference)
    }
    pub fn with_failure_ref(self, failure_ref: &str) -> Self {
        let Some(id) = self
            .operation
            .as_ref()
            .filter(|operation| operation.is_active())
            .and_then(|operation| operation.operation_id())
            .map(str::to_owned)
        else {
            return self;
        };
        self.with_evidence_reference(
            failure_ref,
            &crate::EvidenceScope::Operation { operation_id: id },
        )
    }
    pub(crate) fn overlay(
        &self,
        other: &Self,
        budget: &Arc<crate::operations::OperationBudget>,
    ) -> Self {
        let mut data = self.as_value().as_object().cloned().unwrap_or_default();
        if let Some(values) = other.as_value().as_object() {
            for (key, value) in values {
                if data.len() < 256 {
                    data.insert(key.clone(), value.clone());
                }
            }
        }
        let mut result = Self::new(&Value::Object(data));
        result.operation = self.operation.clone();
        result.correlation = self.correlation.clone();
        #[cfg(any(feature = "tower", feature = "actix-web"))]
        {
            result.http = self.http.clone();
        }
        if other.operation.as_ref().is_none_or(|op| op.matches(budget)) {
            result.operation = other.operation.clone().or(result.operation);
            result.correlation.extend(
                other
                    .correlation
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone())),
            );
            #[cfg(any(feature = "tower", feature = "actix-web"))]
            {
                result.http = other.http.clone().or(result.http);
            }
        }
        result
    }
    pub(crate) fn retained_bytes(&self) -> usize {
        let bytes = crate::wire::canonical_size(self.as_value()).unwrap_or(262144)
            + crate::wire::canonical_size(&Value::Object(self.correlation.clone()))
                .unwrap_or(262144);
        #[cfg(any(feature = "tower", feature = "actix-web"))]
        {
            bytes
                + self
                    .http
                    .as_ref()
                    .map_or(0, |request| request.retained_bytes())
        }
        #[cfg(not(any(feature = "tower", feature = "actix-web")))]
        bytes
    }
    pub(crate) fn is_empty(&self) -> bool {
        let empty = self
            .as_value()
            .as_object()
            .is_some_and(serde_json::Map::is_empty)
            && self.correlation.is_empty()
            && self.operation.is_none();
        #[cfg(any(feature = "tower", feature = "actix-web"))]
        {
            empty && self.http.is_none()
        }
        #[cfg(not(any(feature = "tower", feature = "actix-web")))]
        empty
    }
    pub(crate) fn protect_with(mut self, privacy: &Privacy) -> Self {
        self.data = Arc::new(
            privacy
                .protect(self.as_value())
                .filter(Value::is_object)
                .unwrap_or_else(|| json!({})),
        );
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warning,
    Error,
    Critical,
}
impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Critical => "critical",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "trace" => Some(Self::Trace),
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" | "warning" => Some(Self::Warning),
            "error" => Some(Self::Error),
            "fatal" | "critical" => Some(Self::Critical),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum TraceOrigin {
    Capture,
    RuntimeReported,
    Unavailable,
    Unknown,
}
impl TraceOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::RuntimeReported => "runtime_reported",
            Self::Unavailable => "unavailable",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub enum FrameClassification {
    Application,
    Framework,
    Sdk,
    #[default]
    Unknown,
}
impl FrameClassification {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Application => "application",
            Self::Framework => "framework",
            Self::Sdk => "sdk",
            Self::Unknown => "unknown",
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct FrameSnapshot {
    pub function: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub classification: FrameClassification,
}

#[derive(Clone, Debug)]
pub struct ExceptionSnapshot {
    name: String,
    message: String,
    stack: Option<String>,
    graph: Value,
}
impl ExceptionSnapshot {
    pub fn new(name: &str, message: &str) -> Self {
        let truncated = name.len() > 256 || message.len() > 2048;
        let name = text(
            if name.len() <= 16384 && name.trim().is_empty() {
                "RustError"
            } else {
                name
            },
            256,
        );
        let message = text(
            if message.len() <= 16384 && message.trim().is_empty() {
                "Rust error"
            } else {
                message
            },
            16384,
        );
        let graph = json!({"version":1,"runtime":"unknown","root_id":0,"nodes":[{"id":0,"name":name,"message":text(&message,2048),"stack":null,"trace_origin":"unavailable","parsing_status":"unavailable","frames":[],"relationships":[]}],"truncated":false,"unavailable_reason":null});
        let mut snapshot = Self {
            name,
            message,
            stack: None,
            graph,
        };
        if truncated {
            snapshot.mark_truncated();
        }
        snapshot
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn stack(&self) -> Option<&str> {
        self.stack.as_deref()
    }
    pub fn evidence(&self) -> &Value {
        &self.graph
    }
    pub(crate) fn mark_unsafe(&mut self) {
        self.graph["unavailable_reason"] = json!("unsafe");
    }
    pub fn with_stack(mut self, stack: &str, origin: TraceOrigin) -> Self {
        if stack.len() > 16384 {
            self.mark_truncated();
        }
        let stack = text(stack, 16384);
        if !stack.trim().is_empty() {
            self.graph["nodes"][0]["stack"] = Value::String(stack.clone());
            self.graph["nodes"][0]["trace_origin"] = Value::String(origin.as_str().into());
            self.graph["nodes"][0]["parsing_status"] = Value::String("unparsed".into());
            self.stack = Some(stack);
        }
        self.fit();
        self
    }
    pub fn with_cause(mut self, cause: &Self) -> Self {
        // Keep an earlier cause and allow aggregates beside the single causal
        // edge. Every addition must preserve reachability and the shared limits.
        if !self.graph["nodes"][0]["relationships"]
            .as_array()
            .is_some_and(|edges| {
                !edges
                    .iter()
                    .any(|edge| edge["kind"] == "cause" || edge["kind"] == "context")
            })
            || !self.append_graph(cause, "cause")
        {
            self.mark_truncated();
        }
        self.fit();
        self
    }
    /// Copies supplied structured frames. This never captures or symbolises a
    /// stack and does not claim these frames identify where an error was thrown.
    pub fn with_frames(mut self, frames: &[FrameSnapshot], origin: TraceOrigin) -> Self {
        if frames.is_empty() {
            return self;
        }
        let truncated = frames.len() > 32
            || frames.iter().take(32).any(|frame| {
                frame
                    .function
                    .as_ref()
                    .is_some_and(|value| value.len() > 256)
                    || frame.file.as_ref().is_some_and(|value| value.len() > 2048)
            });
        let values=frames.iter().take(32).map(|frame|json!({"function":frame.function.as_ref().map(|value|text(value,256)),"file":frame.file.as_ref().map(|value|text(value,2048)),"line":frame.line.filter(|line|*line>0),"column":frame.column,"classification":frame.classification.as_str()})).collect::<Vec<_>>();
        self.graph["nodes"][0]["frames"] = Value::Array(values);
        self.graph["nodes"][0]["trace_origin"] = json!(origin.as_str());
        self.graph["nodes"][0]["parsing_status"] =
            json!(if truncated { "truncated" } else { "parsed" });
        if truncated {
            self.mark_truncated();
        }
        self.fit();
        self
    }
    pub fn with_aggregate(mut self, children: &[ExceptionSnapshot]) -> Self {
        let mut accepted = 0;
        for child in children.iter().take(5) {
            // Snapshot graphs are immutable owned data, so copying their edges
            // cannot create a cycle. Validate the combined depth/bytes as well
            // as node/edge counts before committing each entire child graph.
            if self.append_graph(child, "aggregate") {
                accepted += 1;
            }
        }
        if children.len() > accepted {
            self.mark_truncated();
        }
        self.fit();
        self
    }
    fn append_graph(&mut self, child: &Self, kind: &str) -> bool {
        let mut candidate = self.clone();
        candidate.inherit_status(child);
        let Some(nodes) = candidate.graph["nodes"].as_array_mut() else {
            return false;
        };
        let Some(children) = child.graph["nodes"].as_array() else {
            return false;
        };
        let offset = nodes.len() as u64;
        if nodes.len() + children.len() > 8 {
            return false;
        }
        let Some(edges) = nodes
            .first_mut()
            .and_then(|node| node["relationships"].as_array_mut())
        else {
            return false;
        };
        if edges.len() >= 5 {
            return false;
        }
        edges.push(json!({"kind":kind,"target_id":offset}));
        for source in children {
            let mut node = source.clone();
            let Some(id) = node["id"].as_u64() else {
                return false;
            };
            node["id"] = json!(id + offset);
            let Some(edges) = node["relationships"].as_array_mut() else {
                return false;
            };
            for edge in edges {
                let Some(id) = edge["target_id"].as_u64() else {
                    return false;
                };
                edge["target_id"] = json!(id + offset);
            }
            nodes.push(node);
        }
        if !crate::wire::valid_graph(&candidate.graph) {
            return false;
        }
        self.graph = candidate.graph;
        true
    }
    fn mark_truncated(&mut self) {
        self.graph["truncated"] = json!(true);
        if self.graph["unavailable_reason"] != "unsafe" {
            self.graph["unavailable_reason"] = json!("budget_exceeded");
        }
    }
    fn inherit_status(&mut self, child: &Self) {
        if child.graph["truncated"] == true {
            self.mark_truncated();
        }
        if child.graph["unavailable_reason"] == "unsafe" {
            self.mark_unsafe();
        }
    }
    fn fit(&mut self) {
        if !crate::wire::valid_graph(&self.graph) {
            let unsafe_input = self.graph["unavailable_reason"] == "unsafe";
            self.graph = Self::new(&self.name, &self.message).graph;
            self.mark_truncated();
            if unsafe_input {
                self.mark_unsafe();
            }
        }
    }
}

pub enum ExceptionInput<'a> {
    Snapshot(&'a ExceptionSnapshot),
    Borrowed { type_name: &'static str },
}
mod sealed {
    pub trait Sealed {}
}
/// Sealed to keep application conversions and formatters off capture callers.
pub trait ExceptionSource: sealed::Sealed {
    fn snapshot(&self) -> ExceptionSnapshot;
}
impl sealed::Sealed for ExceptionInput<'_> {}
impl ExceptionSource for ExceptionInput<'_> {
    fn snapshot(&self) -> ExceptionSnapshot {
        ExceptionInput::snapshot(self)
    }
}
impl sealed::Sealed for &ExceptionSnapshot {}
impl ExceptionSource for &ExceptionSnapshot {
    fn snapshot(&self) -> ExceptionSnapshot {
        (*self).clone()
    }
}
impl<'a> From<&'a ExceptionSnapshot> for ExceptionInput<'a> {
    fn from(value: &'a ExceptionSnapshot) -> Self {
        Self::Snapshot(value)
    }
}
impl ExceptionInput<'_> {
    pub fn borrowed<E: Error + ?Sized>(_: &E) -> Self {
        Self::Borrowed {
            type_name: std::any::type_name::<E>(),
        }
    }
    pub fn snapshot(&self) -> ExceptionSnapshot {
        match self {
            Self::Snapshot(s) => (*s).clone(),
            Self::Borrowed { type_name } => {
                let mut snapshot = ExceptionSnapshot::new(
                    type_name,
                    "Error details unavailable without a safe snapshot",
                );
                snapshot.mark_unsafe();
                snapshot
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct RequestSnapshot {
    pub method: String,
    pub path: String,
    pub route_template: Option<String>,
    pub headers: Value,
    pub body: Option<Value>,
    pub status: u16,
    pub duration_ms: f64,
}
impl Default for RequestSnapshot {
    fn default() -> Self {
        Self {
            method: "UNKNOWN".into(),
            path: "/".into(),
            route_template: None,
            headers: json!({}),
            body: None,
            status: 0,
            duration_ms: 0.0,
        }
    }
}
#[derive(Clone, Debug)]
pub struct DeploySnapshot {
    pub commit_sha: String,
    pub version: String,
    pub branch: String,
    pub deployed_at: String,
}
