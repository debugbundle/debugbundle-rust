//! Schema-aware boundary protection for closed SDK-owned protocol structures.
use crate::privacy::Privacy;
use serde_json::{json, Map, Value};

const ALLOWED_HTML_RESOURCE_TAGS: &[&str] = &[
    "script", "link", "img", "audio", "video", "source", "iframe", "object", "embed", "input",
];
const ALLOWED_READY_STATES: &[&str] = &["loading", "interactive", "complete"];
const ALLOWED_VISIBILITY_STATES: &[&str] = &["visible", "hidden", "prerender", "unloaded"];
const ALLOWED_REJECTION_KINDS: &[&str] =
    &["error", "string", "object", "null", "undefined", "unknown"];
const ALLOWED_REJECTION_KEYS: &[&str] = &["kind", "name", "message", "preview"];
const ALLOWED_EVIDENCE_LIMIT_KEYS: &[&str] = &[
    "version",
    "exception_evidence",
    "execution",
    "resource_observations",
    "probe_data",
];
const ALLOWED_EVIDENCE_LIMIT_REASONS: &[&str] =
    &["budget_exceeded", "unsafe", "unsupported", "unknown"];
const ALLOWED_BROWSER_EVENT_KEYS: &[&str] = &[
    "kind",
    "message",
    "file_name",
    "line_number",
    "column_number",
    "target",
    "page",
    "opaque",
];
const ALLOWED_TARGET_KEYS: &[&str] = &["tag_name", "source_url", "attributes"];
const ALLOWED_TARGET_ATTR_STRING_KEYS: &[&str] = &["rel", "as", "type", "media", "cross_origin"];
const ALLOWED_TARGET_ATTR_BOOL_KEYS: &[&str] = &["async", "defer", "integrity_present"];
const ALLOWED_TARGET_ATTR_KEYS: &[&str] = &[
    "rel",
    "as",
    "type",
    "media",
    "cross_origin",
    "async",
    "defer",
    "integrity_present",
];
const ALLOWED_PAGE_KEYS: &[&str] = &["url", "referrer", "ready_state", "visibility_state"];
const ALLOWED_EXCEPTION_EVIDENCE_KEYS: &[&str] = &[
    "version",
    "runtime",
    "root_id",
    "nodes",
    "truncated",
    "unavailable_reason",
];
const ALLOWED_RUNTIMES: &[&str] = &[
    "node",
    "browser",
    "python",
    "php",
    "jvm",
    "go",
    "ruby",
    "dotnet",
    "android",
    "swift",
    "react_native",
    "unknown",
];
const ALLOWED_UNAVAILABLE_REASONS: &[&str] =
    &["unknown", "unsupported", "unsafe", "budget_exceeded"];
const ALLOWED_NODE_KEYS: &[&str] = &[
    "id",
    "name",
    "message",
    "stack",
    "trace_origin",
    "parsing_status",
    "frames",
    "relationships",
];
const ALLOWED_TRACE_ORIGINS: &[&str] = &[
    "throw",
    "native_fatal",
    "capture",
    "runtime_reported",
    "unavailable",
    "unknown",
];
const ALLOWED_PARSING_STATUSES: &[&str] =
    &["parsed", "partial", "unparsed", "unavailable", "truncated"];
const ALLOWED_FRAME_KEYS: &[&str] = &["function", "file", "line", "column", "classification"];
const ALLOWED_CLASSIFICATIONS: &[&str] = &["application", "framework", "sdk", "unknown"];
const ALLOWED_EDGE_KEYS: &[&str] = &["kind", "target_id"];
const ALLOWED_EDGE_KINDS: &[&str] = &["cause", "context", "aggregate", "suppressed"];
const ALLOWED_EXECUTION_KEYS: &[&str] = &["version", "kind", "name", "elapsed_ms", "limit_ms"];
const ALLOWED_EXECUTION_KINDS: &[&str] = &["request", "command", "job", "unknown"];
const ALLOWED_RESOURCE_OBS_KEYS: &[&str] = &["version", "items"];
const ALLOWED_TIMING_KEYS: &[&str] = &[
    "kind",
    "observed_at",
    "source_url",
    "availability",
    "response_status",
    "duration_ms",
    "transfer_size_bytes",
];
const ALLOWED_AVAILABILITIES: &[&str] = &[
    "observed",
    "unsupported",
    "no_match",
    "withheld",
    "ambiguous",
    "unknown",
];
const ALLOWED_CSP_KEYS: &[&str] = &[
    "kind",
    "observed_at",
    "source_url",
    "effective_directive",
    "disposition",
    "document_status_code",
];
const ALLOWED_DISPOSITIONS: &[&str] = &["enforce", "report"];
const ALLOWED_RECOVERY_KEYS: &[&str] = &[
    "version",
    "kind",
    "failure_ref",
    "scope",
    "attempt_id",
    "state",
    "operation",
    "status_code",
    "duration_ms",
];
const ALLOWED_RECOVERY_STATES: &[&str] = &[
    "attempt_started",
    "attempt_completed",
    "attempt_failed",
    "recovery_observed",
    "failure_persisted",
];
const ALLOWED_RESOURCE_DIAG_KEYS: &[&str] = &[
    "version",
    "kind",
    "failure_ref",
    "scope",
    "source_event_id",
    "observations",
];

fn only_keys(map: &Map<String, Value>, allowed: &[&str]) -> bool {
    map.keys().all(|key| allowed.contains(&key.as_str()))
}

fn scrub_protocol_string(privacy: &Privacy, value: &str) -> Option<String> {
    privacy
        .protect(&Value::String(value.to_owned()))
        .and_then(|protected| protected.as_str().map(str::to_owned))
}

fn scrub_nullable_string(privacy: &Privacy, value: &Value) -> Option<Value> {
    match value {
        Value::Null => Some(Value::Null),
        Value::String(text) => scrub_protocol_string(privacy, text).map(Value::String),
        _ => None,
    }
}

fn non_negative_number(value: &Value) -> Option<Value> {
    let number = value.as_number()?;
    super::number_size(number)?;
    let float = number.as_f64()?;
    (float.is_finite() && float >= 0.0).then(|| value.clone())
}

fn valid_evidence_limits(value: &Value) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    if !only_keys(map, ALLOWED_EVIDENCE_LIMIT_KEYS)
        || map.get("version").and_then(super::unsigned) != Some(1)
    {
        return false;
    }
    map.iter().all(|(key, reason)| {
        key == "version"
            || reason
                .as_str()
                .is_some_and(|s| ALLOWED_EVIDENCE_LIMIT_REASONS.contains(&s))
    })
}

fn valid_probe_limits(value: &Value) -> bool {
    value.as_object().is_some_and(|map| {
        map.len() == 2
            && map.get("version").and_then(super::unsigned) == Some(1)
            && map.get("reason").and_then(Value::as_str) == Some("budget_exceeded")
    })
}

fn sanitize_browser_event_block(privacy: &Privacy, raw: &Value) -> Option<Value> {
    let map = raw.as_object()?;
    if !only_keys(map, ALLOWED_BROWSER_EVENT_KEYS) {
        return None;
    }
    let kind = map.get("kind")?.as_str()?;
    if kind != "window_error" && kind != "resource_error" {
        return None;
    }
    let opaque = map.get("opaque")?.as_bool()?;
    let line_number = match map.get("line_number")? {
        Value::Null => Value::Null,
        value => {
            super::unsigned(value)?;
            value.clone()
        }
    };
    let column_number = match map.get("column_number")? {
        Value::Null => Value::Null,
        value => {
            super::unsigned(value)?;
            value.clone()
        }
    };
    let message = scrub_nullable_string(privacy, map.get("message")?)?;
    let file_name = scrub_nullable_string(privacy, map.get("file_name")?)?;

    let target = match map.get("target")? {
        Value::Null => Value::Null,
        Value::Object(raw_target) => {
            if !only_keys(raw_target, ALLOWED_TARGET_KEYS) {
                return None;
            }
            let tag_name = match raw_target.get("tag_name")? {
                Value::Null => Value::Null,
                Value::String(tag) => {
                    if ALLOWED_HTML_RESOURCE_TAGS.contains(&tag.to_ascii_lowercase().as_str()) {
                        Value::String(tag.clone())
                    } else {
                        let sanitized = privacy.protect(&json!({"tag_name": tag}))?;
                        Value::String(sanitized.get("tag_name")?.as_str()?.to_owned())
                    }
                }
                _ => return None,
            };
            let source_url = scrub_nullable_string(privacy, raw_target.get("source_url")?)?;
            let mut target_out = Map::new();
            target_out.insert("tag_name".into(), tag_name);
            target_out.insert("source_url".into(), source_url);
            if let Some(raw_attrs) = raw_target.get("attributes") {
                let attrs = raw_attrs.as_object()?;
                if !only_keys(attrs, ALLOWED_TARGET_ATTR_KEYS) {
                    return None;
                }
                let mut attrs_out = Map::new();
                for key in ALLOWED_TARGET_ATTR_STRING_KEYS {
                    if let Some(value) = attrs.get(*key) {
                        let text = value.as_str()?;
                        attrs_out.insert(
                            (*key).into(),
                            Value::String(scrub_protocol_string(privacy, text)?),
                        );
                    }
                }
                for key in ALLOWED_TARGET_ATTR_BOOL_KEYS {
                    if let Some(value) = attrs.get(*key) {
                        attrs_out.insert((*key).into(), Value::Bool(value.as_bool()?));
                    }
                }
                target_out.insert("attributes".into(), Value::Object(attrs_out));
            }
            Value::Object(target_out)
        }
        _ => return None,
    };

    let mut out = Map::new();
    out.insert("kind".into(), Value::String(kind.into()));
    out.insert("message".into(), message);
    out.insert("file_name".into(), file_name);
    out.insert("line_number".into(), line_number);
    out.insert("column_number".into(), column_number);
    out.insert("target".into(), target);
    out.insert("opaque".into(), Value::Bool(opaque));

    if let Some(raw_page) = map.get("page") {
        let page = raw_page.as_object()?;
        if !only_keys(page, ALLOWED_PAGE_KEYS) {
            return None;
        }
        let url = scrub_nullable_string(privacy, page.get("url")?)?;
        let referrer = scrub_nullable_string(privacy, page.get("referrer")?)?;
        let ready_state = match page.get("ready_state")? {
            Value::Null => Value::Null,
            Value::String(s) if ALLOWED_READY_STATES.contains(&s.as_str()) => {
                Value::String(s.clone())
            }
            _ => return None,
        };
        let visibility_state = match page.get("visibility_state")? {
            Value::Null => Value::Null,
            Value::String(s) if ALLOWED_VISIBILITY_STATES.contains(&s.as_str()) => {
                Value::String(s.clone())
            }
            _ => return None,
        };
        let mut page_out = Map::new();
        page_out.insert("url".into(), url);
        page_out.insert("referrer".into(), referrer);
        page_out.insert("ready_state".into(), ready_state);
        page_out.insert("visibility_state".into(), visibility_state);
        out.insert("page".into(), Value::Object(page_out));
    }

    Some(Value::Object(out))
}

fn sanitize_rejection_reason_block(privacy: &Privacy, raw: &Value) -> Option<Value> {
    let map = raw.as_object()?;
    if !only_keys(map, ALLOWED_REJECTION_KEYS) {
        return None;
    }
    let kind = map.get("kind")?.as_str()?;
    if !ALLOWED_REJECTION_KINDS.contains(&kind) {
        return None;
    }
    let mut out = Map::new();
    out.insert("kind".into(), Value::String(kind.into()));
    for field in ["name", "message", "preview"] {
        if let Some(value) = map.get(field) {
            let text = value.as_str()?;
            if text.is_empty() {
                return None;
            }
            out.insert(
                field.into(),
                Value::String(scrub_protocol_string(privacy, text)?),
            );
        }
    }
    Some(Value::Object(out))
}

fn sanitize_exception_evidence_block(privacy: &Privacy, raw: &Value) -> Option<Value> {
    let map = raw.as_object()?;
    if !only_keys(map, ALLOWED_EXCEPTION_EVIDENCE_KEYS)
        || !super::valid_graph_shape(raw, false)
        || map.get("version").and_then(super::unsigned) != Some(1)
    {
        return None;
    }
    let runtime = map.get("runtime")?.as_str()?;
    if !ALLOWED_RUNTIMES.contains(&runtime) {
        return None;
    }
    let root_id = map.get("root_id")?;
    if super::unsigned(root_id)? > 7 {
        return None;
    }
    let truncated = map.get("truncated")?.as_bool()?;
    let unavailable_reason = match map.get("unavailable_reason")? {
        Value::Null => Value::Null,
        Value::String(s) if ALLOWED_UNAVAILABLE_REASONS.contains(&s.as_str()) => {
            Value::String(s.clone())
        }
        _ => return None,
    };
    let raw_nodes = map.get("nodes")?.as_array()?;
    let mut nodes = Vec::with_capacity(raw_nodes.len());
    for raw_node in raw_nodes {
        let node = raw_node.as_object()?;
        if !only_keys(node, ALLOWED_NODE_KEYS) {
            return None;
        }
        let id = node.get("id")?;
        if super::unsigned(id)? > 7 {
            return None;
        }
        let name = scrub_protocol_string(privacy, node.get("name")?.as_str()?)?;
        let message = scrub_protocol_string(privacy, node.get("message")?.as_str()?)?;
        let stack = scrub_nullable_string(privacy, node.get("stack")?)?;
        let trace_origin = node.get("trace_origin")?.as_str()?;
        let parsing_status = node.get("parsing_status")?.as_str()?;
        if !ALLOWED_TRACE_ORIGINS.contains(&trace_origin)
            || !ALLOWED_PARSING_STATUSES.contains(&parsing_status)
        {
            return None;
        }
        let raw_frames = node.get("frames")?.as_array()?;
        let mut frames = Vec::with_capacity(raw_frames.len());
        for raw_frame in raw_frames {
            let frame = raw_frame.as_object()?;
            if !only_keys(frame, ALLOWED_FRAME_KEYS) {
                return None;
            }
            let function = scrub_nullable_string(privacy, frame.get("function")?)?;
            let file = scrub_nullable_string(privacy, frame.get("file")?)?;
            let line = match frame.get("line")? {
                Value::Null => Value::Null,
                value => {
                    if super::unsigned(value)? == 0 {
                        return None;
                    }
                    value.clone()
                }
            };
            let column = match frame.get("column")? {
                Value::Null => Value::Null,
                value => {
                    super::unsigned(value)?;
                    value.clone()
                }
            };
            let classification = frame.get("classification")?.as_str()?;
            if !ALLOWED_CLASSIFICATIONS.contains(&classification) {
                return None;
            }
            frames.push(json!({
                "function": function,
                "file": file,
                "line": line,
                "column": column,
                "classification": classification,
            }));
        }
        let raw_edges = node.get("relationships")?.as_array()?;
        let mut relationships = Vec::with_capacity(raw_edges.len());
        for raw_edge in raw_edges {
            let edge = raw_edge.as_object()?;
            if !only_keys(edge, ALLOWED_EDGE_KEYS) {
                return None;
            }
            let kind = edge.get("kind")?.as_str()?;
            let target_id = edge.get("target_id")?;
            if !ALLOWED_EDGE_KINDS.contains(&kind) || super::unsigned(target_id)? > 7 {
                return None;
            }
            relationships.push(json!({"kind": kind, "target_id": target_id}));
        }
        nodes.push(json!({
            "id": id,
            "name": name,
            "message": message,
            "stack": stack,
            "trace_origin": trace_origin,
            "parsing_status": parsing_status,
            "frames": frames,
            "relationships": relationships,
        }));
    }
    Some(json!({
        "version": map.get("version")?,
        "runtime": runtime,
        "root_id": root_id,
        "nodes": nodes,
        "truncated": truncated,
        "unavailable_reason": unavailable_reason,
    }))
}

fn sanitize_execution_block(privacy: &Privacy, raw: &Value) -> Option<Value> {
    let map = raw.as_object()?;
    if !only_keys(map, ALLOWED_EXECUTION_KEYS)
        || map.get("version").and_then(super::unsigned) != Some(1)
    {
        return None;
    }
    let kind = map.get("kind")?.as_str()?;
    if !ALLOWED_EXECUTION_KINDS.contains(&kind) {
        return None;
    }
    let name = match map.get("name")? {
        Value::Null => Value::Null,
        Value::String(s) if s.len() <= 128 => Value::String(scrub_protocol_string(privacy, s)?),
        _ => return None,
    };
    let elapsed_ms = match map.get("elapsed_ms")? {
        Value::Null => Value::Null,
        value => non_negative_number(value)?,
    };
    let limit_ms = match map.get("limit_ms")? {
        Value::Null => Value::Null,
        value => non_negative_number(value)?,
    };
    Some(json!({
        "version": map.get("version")?,
        "kind": kind,
        "name": name,
        "elapsed_ms": elapsed_ms,
        "limit_ms": limit_ms,
    }))
}

fn sanitize_resource_observations_block(privacy: &Privacy, raw: &Value) -> Option<Value> {
    let map = raw.as_object()?;
    if !only_keys(map, ALLOWED_RESOURCE_OBS_KEYS)
        || map.get("version").and_then(super::unsigned) != Some(1)
    {
        return None;
    }
    let raw_items = map.get("items")?.as_array()?;
    if raw_items.is_empty() || raw_items.len() > 5 {
        return None;
    }
    let mut items = Vec::with_capacity(raw_items.len());
    for raw_item in raw_items {
        let item = raw_item.as_object()?;
        if !super::timestamp(item.get("observed_at")) {
            return None;
        }
        let observed_at = item.get("observed_at")?.clone();
        let source_url = scrub_nullable_string(privacy, item.get("source_url")?)?;
        match item.get("kind")?.as_str()? {
            "timing" => {
                if !only_keys(item, ALLOWED_TIMING_KEYS) {
                    return None;
                }
                let availability = item.get("availability")?.as_str()?;
                if !ALLOWED_AVAILABILITIES.contains(&availability) {
                    return None;
                }
                let response_status = match item.get("response_status")? {
                    Value::Null => Value::Null,
                    value => {
                        let code = super::unsigned(value)?;
                        if !(100..=599).contains(&code) {
                            return None;
                        }
                        value.clone()
                    }
                };
                let duration_ms = match item.get("duration_ms")? {
                    Value::Null => Value::Null,
                    value => non_negative_number(value)?,
                };
                let transfer_size_bytes = match item.get("transfer_size_bytes")? {
                    Value::Null => Value::Null,
                    value => {
                        super::unsigned(value)?;
                        value.clone()
                    }
                };
                items.push(json!({
                    "kind": "timing",
                    "observed_at": observed_at,
                    "source_url": source_url,
                    "availability": availability,
                    "response_status": response_status,
                    "duration_ms": duration_ms,
                    "transfer_size_bytes": transfer_size_bytes,
                }));
            }
            "csp" => {
                if !only_keys(item, ALLOWED_CSP_KEYS) {
                    return None;
                }
                let effective_directive = item.get("effective_directive")?.as_str()?;
                let disposition = item.get("disposition")?.as_str()?;
                if effective_directive.is_empty()
                    || effective_directive.len() > 64
                    || !effective_directive
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b == b'-')
                    || !ALLOWED_DISPOSITIONS.contains(&disposition)
                {
                    return None;
                }
                let document_status_code = match item.get("document_status_code")? {
                    Value::Null => Value::Null,
                    value => {
                        let code = super::unsigned(value)?;
                        if !(100..=599).contains(&code) {
                            return None;
                        }
                        value.clone()
                    }
                };
                items.push(json!({
                    "kind": "csp",
                    "observed_at": observed_at,
                    "source_url": source_url,
                    "effective_directive": effective_directive,
                    "disposition": disposition,
                    "document_status_code": document_status_code,
                }));
            }
            _ => return None,
        }
    }
    let out = json!({"version": map.get("version")?, "items": items});
    super::valid_resources(&out).then_some(out)
}

fn sanitize_diagnostic_context_payload(privacy: &Privacy, raw: &Value) -> Option<Value> {
    let map = raw.as_object()?;
    if map.get("version").and_then(super::unsigned) != Some(1)
        || !super::v4(map.get("failure_ref")?)
        || !super::valid_scope(map.get("scope")?)
    {
        return None;
    }
    match map.get("kind")?.as_str()? {
        "recovery" => {
            if !only_keys(map, ALLOWED_RECOVERY_KEYS) || !super::v4(map.get("attempt_id")?) {
                return None;
            }
            let state = map.get("state")?.as_str()?;
            if !ALLOWED_RECOVERY_STATES.contains(&state) {
                return None;
            }
            let mut out = Map::new();
            out.insert("version".into(), map.get("version")?.clone());
            out.insert("kind".into(), json!("recovery"));
            out.insert("failure_ref".into(), map.get("failure_ref")?.clone());
            out.insert("scope".into(), map.get("scope")?.clone());
            out.insert("attempt_id".into(), map.get("attempt_id")?.clone());
            out.insert("state".into(), Value::String(state.into()));
            if let Some(op) = map.get("operation") {
                let text = op.as_str()?;
                if text.len() > 64 {
                    return None;
                }
                out.insert(
                    "operation".into(),
                    Value::String(scrub_protocol_string(privacy, text)?),
                );
            }
            if let Some(status) = map.get("status_code") {
                let code = super::unsigned(status)?;
                if !(100..=599).contains(&code) {
                    return None;
                }
                out.insert("status_code".into(), status.clone());
            }
            if let Some(duration) = map.get("duration_ms") {
                out.insert("duration_ms".into(), non_negative_number(duration)?);
            }
            Some(Value::Object(out))
        }
        "resource_observation" => {
            if !only_keys(map, ALLOWED_RESOURCE_DIAG_KEYS)
                || !super::v4(map.get("source_event_id")?)
            {
                return None;
            }
            let observations =
                sanitize_resource_observations_block(privacy, map.get("observations")?)?;
            Some(json!({
                "version": map.get("version")?,
                "kind": "resource_observation",
                "failure_ref": map.get("failure_ref")?,
                "scope": map.get("scope")?,
                "source_event_id": map.get("source_event_id")?,
                "observations": observations,
            }))
        }
        _ => None,
    }
}

/// Protect application context while preserving validated closed SDK-owned metadata.
pub(crate) fn protect_context(privacy: &Privacy, value: &Value) -> Option<Value> {
    let Some(map) = value.as_object() else {
        return privacy.protect(value);
    };
    if map.len() > 256 {
        return privacy.protect(value);
    }
    let mut raw = map.clone();
    let mut preserved = Map::new();
    if let Some(reference) = raw.get("evidence_reference") {
        if super::valid_reference(reference) {
            preserved.insert("evidence_reference".into(), reference.clone());
            raw.remove("evidence_reference");
        }
    }
    if let Some(limits) = raw.get("evidence_limits") {
        if valid_evidence_limits(limits) {
            preserved.insert("evidence_limits".into(), limits.clone());
            raw.remove("evidence_limits");
        }
    }
    if let Some(limits) = raw.get("probe_limits") {
        if valid_probe_limits(limits) {
            preserved.insert("probe_limits".into(), limits.clone());
            raw.remove("probe_limits");
        }
    }
    let mut protected = privacy.protect(&Value::Object(raw))?;
    if !preserved.is_empty() {
        let out = protected.as_object_mut()?;
        out.extend(preserved);
        super::native_size(&protected, super::MAX_BODY_BYTES)?;
    }
    Some(protected)
}

fn protect_event_payload(
    privacy: &Privacy,
    event_type: &str,
    payload: &Value,
) -> Option<(Value, bool)> {
    if event_type == "diagnostic_context" {
        return sanitize_diagnostic_context_payload(privacy, payload).map(|value| (value, false));
    }
    let Some(map) = payload.as_object() else {
        return privacy.protect_payload(payload).map(|value| (value, false));
    };
    if map.len() > 256 {
        return privacy.protect_payload(payload).map(|value| (value, false));
    }
    let mut raw = map.clone();
    let mut preserved = Map::new();
    let mut valid_optional = false;
    let mut invalid_optional = false;
    if event_type == "frontend_exception" {
        if let Some(block) = raw.get("browser_event") {
            if let Some(safe) = sanitize_browser_event_block(privacy, block) {
                preserved.insert("browser_event".into(), safe);
                raw.remove("browser_event");
            }
        }
        if let Some(block) = raw.get("rejection_reason") {
            if let Some(safe) = sanitize_rejection_reason_block(privacy, block) {
                preserved.insert("rejection_reason".into(), safe);
                raw.remove("rejection_reason");
            }
        }
        if let Some(block) = raw.get("resource_observations") {
            if let Some(safe) = sanitize_resource_observations_block(privacy, block) {
                preserved.insert("resource_observations".into(), safe);
                raw.remove("resource_observations");
                valid_optional = true;
            } else {
                invalid_optional = true;
            }
        }
    }
    if event_type == "backend_exception" || event_type == "frontend_exception" {
        if let Some(block) = raw.get("exception_evidence") {
            if let Some(safe) = sanitize_exception_evidence_block(privacy, block) {
                preserved.insert("exception_evidence".into(), safe);
                raw.remove("exception_evidence");
                valid_optional = true;
            } else {
                invalid_optional = true;
            }
        }
    }
    if event_type == "backend_exception" {
        if let Some(block) = raw.get("execution") {
            if let Some(safe) = sanitize_execution_block(privacy, block) {
                preserved.insert("execution".into(), safe);
                raw.remove("execution");
                valid_optional = true;
            } else {
                invalid_optional = true;
            }
        }
    }
    if event_type == "error_suppressed"
        && raw.get("reason").and_then(Value::as_str) == Some("queue_pressure")
    {
        preserved.insert("reason".into(), json!("queue_pressure"));
        raw.remove("reason");
    }
    let mut protected = privacy.protect_payload(&Value::Object(raw))?;
    if !preserved.is_empty() {
        let out = protected.as_object_mut()?;
        out.extend(preserved);
    }
    Some((protected, valid_optional && !invalid_optional))
}

/// Protect application fields without letting custom broad rules corrupt
/// validated closed SDK protocol structures or identity/correlation fields.
pub fn protect_event(privacy: &Privacy, event: &Value) -> Option<Value> {
    let reduced;
    let event = if super::preflight(event) {
        event
    } else {
        reduced = super::without_optional(event)?;
        &reduced
    };
    // Clone only the envelope fields that survive this projection. The three
    // protected sections may contain a large graph that is rebuilt below.
    let mut output = Value::Object(
        event
            .as_object()?
            .iter()
            .filter(|(key, _)| {
                !matches!(
                    key.as_str(),
                    "payload" | "context" | "service" | "project_token"
                )
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );
    let event_type = event
        .get("event_type")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut valid_optional = false;
    if let Some(payload) = event.get("payload") {
        let (protected, has_optional) = protect_event_payload(privacy, event_type, payload)?;
        output["payload"] = protected;
        valid_optional = has_optional;
    }
    if let Some(context) = event.get("context") {
        output["context"] = protect_context(privacy, context)?;
    }
    if let Some(service) = event.get("service") {
        output["service"] = privacy.protect(service)?;
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
        for value in correlation.values() {
            if let Some(text) = value.as_str() {
                if privacy.text(text) != text {
                    return None;
                }
            }
            if value.is_array() || value.is_object() {
                return None;
            }
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
    // Optional evidence must also yield at its combined semantic byte limit or
    // when mandatory/custom scrubbing expands an otherwise valid optional field.
    if !super::evidence_within_budget(&output)
        || super::native_size(&output, super::MAX_BODY_BYTES).is_none()
        || (valid_optional && !super::valid_event(&output))
    {
        output = super::without_optional(&output)?;
    }
    super::valid_event(&output).then_some(output)
}
