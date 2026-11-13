//! Closed receiving schema plus semantic checks JSON Schema cannot express.
pub use crate::analytics_wire::valid_event as valid_analytics_event;
use serde_json::{json, Map, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
};

pub const MAX_BODY_BYTES: usize = 256 * 1024;
pub const MAX_EVENT_BYTES: usize = MAX_BODY_BYTES - 13;
pub const FEATURES: [&str; 4] = [
    "exception_evidence_v1",
    "resource_observations_v1",
    "recovery_v1",
    "evidence_provenance_v1",
];

static SCHEMA: LazyLock<Option<jsonschema::Validator>> = LazyLock::new(|| {
    let source: Value =
        serde_json::from_str(include_str!("../tests/fixtures/event-envelope.schema.json")).ok()?;
    jsonschema::options()
        .should_validate_formats(true)
        .build(&source)
        .ok()
});

pub fn valid_event(event: &Value) -> bool {
    preflight(event) && valid_shape(event)
}
pub(crate) fn prepare() -> bool {
    SCHEMA.as_ref().is_some()
}
// Only the bounded callback worker uses this on a hook's replacement. Separating
// schema validity from wire budgets prevents restoring pre-hook content when a
// valid replacement is too large to retain or transmit.
pub(crate) fn valid_shape(event: &Value) -> bool {
    if !SCHEMA.as_ref().is_some_and(|s| s.is_valid(event)) {
        return false;
    }
    if !timestamp(event.get("occurred_at")) {
        return false;
    }
    let p = &event["payload"];
    if let Some(graph) = p.get("exception_evidence") {
        if !valid_graph(graph) {
            return false;
        }
    }
    if p.get("execution").is_some_and(|e| {
        e.get("name")
            .and_then(Value::as_str)
            .is_some_and(|s| s.len() > 128)
    }) {
        return false;
    }
    let mut additions = Map::new();
    for key in ["exception_evidence", "execution", "resource_observations"] {
        if let Some(value) = p.get(key) {
            additions.insert(key.into(), value.clone());
        }
    }
    if !additions.is_empty() {
        let reference = &event["context"]["evidence_reference"];
        if valid_reference(reference) {
            additions.insert("evidence_reference".into(), reference.clone());
        }
        if canonical_size(&Value::Object(additions)).is_none_or(|n| n > 32768) {
            return false;
        }
    }
    if event["event_type"] == "diagnostic_context" {
        if !v4(&p["failure_ref"]) || !valid_scope(&p["scope"]) {
            return false;
        }
        if p["scope"]["kind"] == "page"
            && event["correlation"]["session_id"]
                .as_str()
                .is_none_or(|s| s.trim().is_empty())
        {
            return false;
        }
        if p["kind"] == "recovery"
            && (!v4(&p["attempt_id"]) || p["operation"].as_str().is_some_and(|s| s.len() > 64))
        {
            return false;
        }
        if let Some(resources) = p.get("observations") {
            if !valid_resources(resources) {
                return false;
            }
        }
    }
    if let Some(resources) = p.get("resource_observations") {
        if !valid_resources(resources) {
            return false;
        }
    }
    if event["event_type"] == "frontend_exception" {
        let mobile = [
            "@debugbundle/sdk-android",
            "@debugbundle/sdk-swift",
            "@debugbundle/sdk-react-native",
        ]
        .contains(&event["sdk_name"].as_str().unwrap_or(""));
        if mobile {
            if p.get("device").is_none_or(Value::is_null) {
                return false;
            }
        } else if p.get("browser").is_none() {
            return false;
        }
    }
    if let Some(items) = p["probe_data"]["items"].as_array() {
        if items.iter().any(|item| !timestamp(item.get("timestamp"))) {
            return false;
        }
    }
    if event["event_type"] == "deploy_metadata" && !timestamp(p.get("deployed_at")) {
        return false;
    }
    true
}

pub(crate) fn preflight(value: &Value) -> bool {
    bounded(value, 0, &mut 0, &mut 0)
}
/// Number is serde_json's concrete primitive, whose Display writes its stored
/// representation. Bound that write before cloning or parsing a consumer's
/// arbitrary-precision representation; never invoke an application formatter.
pub(crate) fn number_size(number: &serde_json::Number) -> Option<usize> {
    struct Counter(usize);
    impl std::fmt::Write for Counter {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            if text.len() > 128usize.saturating_sub(self.0) {
                return Err(std::fmt::Error);
            }
            self.0 += text.len();
            Ok(())
        }
    }
    let mut counter = Counter(0);
    std::fmt::write(&mut counter, format_args!("{number}")).ok()?;
    Some(counter.0)
}
/// JSON has one number type. Accept decimal/exponent spellings of mathematical
/// integers exactly as the JavaScript receiving contract does; each caller still
/// enforces its small count/index/version bound.
pub(crate) fn unsigned(value: &Value) -> Option<u64> {
    let number = value.as_number()?;
    number_size(number)?;
    if let Some(integer) = number.as_u64() {
        return Some(integer);
    }
    let number = number.as_f64()?;
    (number.is_finite()
        && number >= 0.0
        && number.fract() == 0.0
        && number <= 9_007_199_254_740_991.0)
        .then_some(number as u64)
}
/// Count concrete JSON bytes without allocating an oversized serialized body.
pub(crate) fn native_size(value: &Value, limit: usize) -> Option<usize> {
    struct Counter {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes) {
                return Err(std::io::ErrorKind::WriteZero.into());
            }
            self.bytes += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    if !preflight(value) {
        return None;
    }
    let mut counter = Counter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value).ok()?;
    Some(counter.bytes)
}

/// Optional additions must yield before a protected primary is lost. This only
/// removes optional fields from the current event, never restores pre-hook data.
pub(crate) fn fit_event(event: &Value, limit: usize) -> Option<Value> {
    let limit = limit.min(MAX_EVENT_BYTES);
    if native_size(event, limit).is_some() {
        return Some(event.clone());
    }
    let mut reduced = without_optional(event)?;
    if native_size(&reduced, limit).is_none() && event["context"].get("evidence_limits").is_none() {
        if let Some(context) = reduced["context"].as_object_mut() {
            context.remove("evidence_limits");
        }
    }
    (native_size(&reduced, limit).is_some() && valid_event(&reduced)).then_some(reduced)
}
fn bounded(value: &Value, depth: usize, nodes: &mut usize, bytes: &mut usize) -> bool {
    *nodes += 1;
    if depth > 32 || *nodes > 8192 {
        return false;
    }
    match value {
        Value::Object(m) => {
            m.len() <= 1024
                && m.iter().all(|(k, v)| {
                    *bytes = bytes.saturating_add(k.len() + 3);
                    *bytes <= MAX_BODY_BYTES && bounded(v, depth + 1, nodes, bytes)
                })
        }
        Value::Array(a) => a.len() <= 1024 && a.iter().all(|v| bounded(v, depth + 1, nodes, bytes)),
        Value::String(s) => {
            *bytes = bytes.saturating_add(s.len() + 2);
            *bytes <= MAX_BODY_BYTES
        }
        Value::Number(number) => {
            let Some(size) = number_size(number) else {
                return false;
            };
            *bytes = bytes.saturating_add(size);
            *bytes <= MAX_BODY_BYTES
        }
        _ => {
            *bytes = bytes.saturating_add(8);
            *bytes <= MAX_BODY_BYTES
        }
    }
}

pub(crate) fn timestamp(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|s| s.ends_with('Z') && chrono::DateTime::parse_from_rfc3339(s).is_ok())
}
fn v4(value: &Value) -> bool {
    value
        .as_str()
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .is_some_and(|u| u.get_version_num() == 4 && u.get_variant() == uuid::Variant::RFC4122)
}
fn valid_scope(scope: &Value) -> bool {
    scope.as_object().is_some_and(|m| m.len() == 2)
        && match scope["kind"].as_str() {
            Some("page") => v4(&scope["page_id"]),
            Some("operation") => v4(&scope["operation_id"]),
            _ => false,
        }
}
pub fn valid_reference(reference: &Value) -> bool {
    reference.as_object().is_some_and(|m| m.len() == 3)
        && unsigned(&reference["version"]) == Some(1)
        && v4(&reference["failure_ref"])
        && valid_scope(&reference["scope"])
}
pub(crate) fn valid_graph(graph: &Value) -> bool {
    let Some(nodes) = graph["nodes"].as_array() else {
        return false;
    };
    // A snapshot is retained under this field in the shared evidence object.
    // Reserve its JSON wrapper too; a graph just below 32 KiB can otherwise
    // pass composition but fail the final event's identical evidence budget.
    if nodes.is_empty()
        || nodes.len() > 8
        || canonical_size(graph).is_none_or(|n| n > 32768 - b"{\"exception_evidence\":}".len())
    {
        return false;
    }
    let mut indexed = HashMap::new();
    for node in nodes {
        let Some(id) = unsigned(&node["id"]) else {
            return false;
        };
        if indexed.insert(id, node).is_some() {
            return false;
        }
        for (name, limit) in [("name", 256), ("message", 2048), ("stack", 16384)] {
            if node[name].as_str().is_some_and(|s| s.len() > limit) {
                return false;
            }
        }
        if let Some(frames) = node["frames"].as_array() {
            if frames.len() > 32
                || frames.iter().any(|f| {
                    f["function"].as_str().is_some_and(|s| s.len() > 256)
                        || f["file"].as_str().is_some_and(|s| s.len() > 2048)
                })
            {
                return false;
            }
        }
        let Some(edges) = node["relationships"].as_array() else {
            return false;
        };
        let mut seen = HashSet::new();
        let mut causal = 0;
        if edges.len() > 5 {
            return false;
        }
        for edge in edges {
            let Some(target) = unsigned(&edge["target_id"]) else {
                return false;
            };
            let kind = edge["kind"].as_str().unwrap_or("");
            if !seen.insert((kind, target)) {
                return false;
            }
            if kind == "cause" || kind == "context" {
                causal += 1;
            }
        }
        if causal > 1 {
            return false;
        }
    }
    fn visit(
        id: u64,
        depth: usize,
        indexed: &HashMap<u64, &Value>,
        active: &mut HashSet<u64>,
        reached: &mut HashSet<u64>,
    ) -> bool {
        if depth > 4 || !active.insert(id) {
            return false;
        }
        let Some(node) = indexed.get(&id) else {
            return false;
        };
        reached.insert(id);
        let Some(edges) = node["relationships"].as_array() else {
            return false;
        };
        for edge in edges {
            if !unsigned(&edge["target_id"])
                .is_some_and(|target| visit(target, depth + 1, indexed, active, reached))
            {
                return false;
            }
        }
        active.remove(&id);
        true
    }
    let Some(root) = unsigned(&graph["root_id"]) else {
        return false;
    };
    let mut reached = HashSet::new();
    visit(root, 0, &indexed, &mut HashSet::new(), &mut reached) && reached.len() == nodes.len()
}
fn valid_resources(resources: &Value) -> bool {
    if canonical_size(resources).is_none_or(|n| n > 32768) {
        return false;
    }
    let Some(items) = resources["items"].as_array() else {
        return false;
    };
    items.iter().all(|item| {
        if !timestamp(item.get("observed_at")) {
            return false;
        }
        if let Some(s) = item["source_url"].as_str() {
            if s.is_empty()
                || s.len() > 2048
                || s.starts_with("//")
                || s.chars()
                    .any(|c| c <= ' ' || c == '\u{7f}' || "\\?#".contains(c))
            {
                return false;
            }
            let parsed = if s.starts_with('/') {
                url::Url::parse(&format!("https://evidence.invalid{s}"))
            } else {
                url::Url::parse(s)
            };
            if !parsed.ok().is_some_and(|u| {
                matches!(u.scheme(), "http" | "https")
                    && u.username().is_empty()
                    && u.password().is_none()
                    && u.query().is_none()
                    && u.fragment().is_none()
            }) {
                return false;
            }
        }
        if let Some(s) = item["effective_directive"].as_str() {
            if s.is_empty()
                || s.len() > 64
                || !s.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
            {
                return false;
            }
        }
        true
    })
}

/// ECMAScript JSON number formatting is required for semantic evidence budgets;
/// native HTTP packing separately counts the actual transmitted serializer bytes.
pub fn canonical_size(value: &Value) -> Option<usize> {
    fn size(v: &Value, depth: usize) -> Option<usize> {
        if depth > 32 {
            return None;
        }
        match v {
            Value::Null => Some(4),
            Value::Bool(b) => Some(if *b { 4 } else { 5 }),
            Value::String(s) => serde_json::to_string(s).ok().map(|s| s.len()),
            Value::Number(n) => {
                let number = n.as_f64()?;
                number
                    .is_finite()
                    .then(|| ryu_js::Buffer::new().format(number).len())
            }
            Value::Array(a) => {
                let mut n = 2usize;
                for (i, v) in a.iter().enumerate() {
                    n = n
                        .checked_add(size(v, depth + 1)?)?
                        .checked_add(usize::from(i > 0))?;
                }
                Some(n)
            }
            Value::Object(m) => {
                let mut n = 2usize;
                for (i, (k, v)) in m.iter().enumerate() {
                    n = n
                        .checked_add(serde_json::to_string(k).ok()?.len())?
                        .checked_add(1)?
                        .checked_add(size(v, depth + 1)?)?
                        .checked_add(usize::from(i > 0))?;
                }
                Some(n)
            }
        }
    }
    preflight(value).then(|| size(value, 0)).flatten()
}
pub fn capabilities(value: &Value) -> Vec<String> {
    parse_capabilities(value).unwrap_or_default()
}
pub(crate) fn valid_descriptor(value: &Value) -> bool {
    parse_capabilities(value).is_some()
}
fn parse_capabilities(value: &Value) -> Option<Vec<String>> {
    let m = value.as_object()?;
    let features = value["features"].as_array()?;
    let mut seen = HashSet::new();
    if m.len() != 2
        || unsigned(&value["version"]) != Some(1)
        || features.len() > 4
        || features.iter().any(|f| {
            !f.as_str()
                .is_some_and(|s| FEATURES.contains(&s) && seen.insert(s))
        })
    {
        return None;
    }
    Some(
        FEATURES
            .iter()
            .filter(|s| seen.contains(**s))
            .map(|s| (*s).to_owned())
            .collect(),
    )
}
pub fn descriptor(features: &[String]) -> Value {
    json!({"version":1,"features":features})
}

/// Protect application fields without treating protocol names such as session_id
/// as application secret keys. Unsafe protocol values still withhold the event.
pub fn protect_event(privacy: &crate::privacy::Privacy, event: &Value) -> Option<Value> {
    let reduced;
    let event = if preflight(event) {
        event
    } else {
        reduced = without_optional(event)?;
        &reduced
    };
    let mut output = event.clone();
    for key in ["payload", "context", "service"] {
        if let Some(value) = event.get(key) {
            output[key] = if key == "payload" {
                privacy.protect_payload(value)?
            } else {
                privacy.protect(value)?
            };
        }
    }
    if event["payload"].get("probe_data").is_some() && output["payload"].get("probe_data").is_none()
    {
        if output.get("context").is_none() {
            output["context"] = json!({});
        }
        if let Some(context) = output["context"]
            .as_object_mut()
            .filter(|map| map.len() < 256)
        {
            context
                .entry("probe_limits")
                .or_insert(json!({"version":1,"reason":"budget_exceeded"}));
        }
    }
    if let Some(correlation) = event.get("correlation").and_then(Value::as_object) {
        for (key, value) in correlation {
            if let Some(text) = value.as_str() {
                if privacy.text(text) != text {
                    return None;
                }
            }
            if value.is_array() || value.is_object() {
                return None;
            }
            output["correlation"][key] = value.clone();
        }
    }
    for key in [
        "schema_version",
        "sdk_name",
        "sdk_version",
        "event_id",
        "occurred_at",
        "project_id",
    ] {
        if let Some(text) = event.get(key).and_then(Value::as_str) {
            if privacy.text(text) != text {
                return None;
            }
        }
    }
    if let Some(root) = output.as_object_mut() {
        root.remove("project_token");
    }
    valid_event(&output).then_some(output)
}

/// Validate only a bounded view; unknown oversized replacements are withheld
/// rather than restoring data the owner's hook may deliberately have removed.
pub(crate) fn hook_replacement(
    privacy: &crate::privacy::Privacy,
    replacement: Value,
    original: Value,
) -> Option<Value> {
    let output = {
        let view = if preflight(&replacement) {
            Some(std::borrow::Cow::Borrowed(&replacement))
        } else {
            without_optional(&replacement).map(std::borrow::Cow::Owned)
        };
        match view {
            Some(view) if valid_shape(&view) => protect_event(privacy, &view),
            Some(_) => Some(original),
            None => None,
        }
    };
    crate::owned_json::discard(replacement);
    output
}

/// Check the reduced view before cloning it: a large hook-owned context cannot
/// force a large SDK allocation just because optional evidence is removable.
fn without_optional(event: &Value) -> Option<Value> {
    if event["event_type"] != "backend_exception" && event["event_type"] != "frontend_exception" {
        return None;
    }
    let root = event.as_object()?;
    let payload = event["payload"].as_object()?;
    let optional = [
        "exception_evidence",
        "execution",
        "resource_observations",
        "probe_data",
    ];
    if !optional.iter().any(|key| payload.contains_key(*key)) {
        return None;
    }
    let typed = valid_reference(&event["context"]["evidence_reference"]);
    let keep = |section: &str, key: &str| {
        !(section == "payload" && optional.contains(&key)
            || section == "context" && key == "evidence_reference" && typed)
    };
    let (mut nodes, mut bytes) = (0, 0);
    for (key, value) in root {
        bytes += key.len() + 3;
        if bytes > MAX_BODY_BYTES {
            return None;
        }
        if key == "payload" || key == "context" {
            let map = value.as_object()?;
            if map.len() > 1024 {
                return None;
            }
            for (name, value) in map {
                if keep(key, name) {
                    bytes += name.len() + 3;
                    if bytes > MAX_BODY_BYTES || !bounded(value, 2, &mut nodes, &mut bytes) {
                        return None;
                    }
                }
            }
        } else if !bounded(value, 1, &mut nodes, &mut bytes) {
            return None;
        }
    }
    let mut output = Map::new();
    for (key, value) in root {
        let cloned = if key == "payload" || key == "context" {
            Value::Object(
                value
                    .as_object()?
                    .iter()
                    .filter(|(name, _)| keep(key, name))
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect(),
            )
        } else {
            value.clone()
        };
        output.insert(key.clone(), cloned);
    }
    let mut output = Value::Object(output);
    let limits = Value::Object(
        optional
            .iter()
            .filter(|key| payload.contains_key(**key))
            .map(|key| ((*key).into(), json!("budget_exceeded")))
            .chain(std::iter::once(("version".into(), json!(1))))
            .collect(),
    );
    if output.get("context").is_none() {
        output["context"] = json!({});
    }
    if let Some(context) = output["context"].as_object_mut() {
        if context.len() < 256 && !context.contains_key("evidence_limits") {
            context.insert("evidence_limits".into(), limits);
        }
    }
    if !preflight(&output) {
        if let Some(context) = output["context"].as_object_mut() {
            context.remove("evidence_limits");
        }
    }
    preflight(&output).then_some(output)
}
