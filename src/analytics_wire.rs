//! Analytics has a separate receiving envelope and semantic privacy refinements.
use serde_json::Value;
use std::sync::LazyLock;
static SCHEMA: LazyLock<Option<jsonschema::Validator>> = LazyLock::new(|| {
    let schema: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/analytics-envelope.schema.json"
    ))
    .ok()?;
    jsonschema::options()
        .should_validate_formats(true)
        .build(&schema)
        .ok()
});
static SENSITIVE_VALUE: LazyLock<Option<regex::Regex>> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)https?://|^[^\s@]+@[^\s@]+\.[^\s@]+$|\bBearer\s+[A-Za-z0-9._~+/=-]+|\b(?:token|password|secret|api_key)=").ok()
});

pub fn valid_event(event: &Value) -> bool {
    if !event.is_object()
        || !crate::wire::preflight(event)
        || !crate::wire::timestamp(event.get("occurred_at"))
    {
        return false;
    }
    // Zod's accepting input schema trims these named text fields. Normalize a
    // bounded validation copy; relay correlation passthrough remains unchanged.
    let mut normalized = event.clone();
    if !normalize(&mut normalized)
        || !SCHEMA
            .as_ref()
            .is_some_and(|schema| schema.is_valid(&normalized))
    {
        return false;
    }
    let payload = &normalized["payload"];
    let kind = payload["kind"].as_str().unwrap_or("");
    for name in ["route", "previous_route"] {
        if let Some(route) = payload.get(name).filter(|route| !route.is_null()) {
            if route["path"].is_null() && route["normalized_path"].is_null() {
                return false;
            }
            if ["path", "normalized_path"].iter().any(|key| {
                route[*key]
                    .as_str()
                    .is_some_and(|path| path.contains(['?', '#']))
            }) {
                return false;
            }
        }
    }
    if ["page_view", "route_change"].contains(&kind) && payload["route"].is_null() {
        return false;
    }
    if kind != "route_change" && !payload["previous_route"].is_null() {
        return false;
    }
    if kind != "session_summary" && payload.get("session").is_some() {
        return false;
    }
    let required: &[&str] = match kind {
        "action" => &["action_key"],
        "funnel_step" => &["funnel_key", "step_key"],
        "conversion" => &["conversion_key"],
        "journey_marker" => &["marker_key"],
        _ => &[],
    };
    if required.iter().any(|key| payload["signal"][*key].is_null()) {
        return false;
    }
    if payload["dimensions"]["referrer_domain"]
        .as_str()
        .is_some_and(|domain| domain.contains(['/', '?', '#', '@']))
    {
        return false;
    }
    if let Some(dimensions) = payload["custom_dimensions"].as_object() {
        if dimensions.len() > 8
            || dimensions.iter().any(|(key, value)| {
                reserved_key(key)
                    || value.as_str().is_some_and(|value| {
                        SENSITIVE_VALUE
                            .as_ref()
                            .is_none_or(|pattern| pattern.is_match(value))
                    })
            })
        {
            return false;
        }
    }
    true
}
fn trim_fields(value: &mut Value, fields: &[&str]) {
    if let Some(map) = value.as_object_mut() {
        for field in fields {
            if let Some(Value::String(text)) = map.get_mut(*field) {
                *text = text.trim().to_owned();
            }
        }
    }
}
fn normalize(event: &mut Value) -> bool {
    trim_fields(event, &["sdk_name", "sdk_version", "project_token"]);
    trim_fields(&mut event["service"], &["name", "environment", "framework"]);
    trim_fields(
        &mut event["correlation"],
        &["session_id", "trace_id", "deploy_id"],
    );
    if let Some(correlation) = event.get_mut("correlation").and_then(Value::as_object_mut) {
        // The source accepts these hexadecimal hashes case-insensitively;
        // JSON Schema regex export omits that flag. Only the validation copy changes.
        for field in ["visitor_id_hash", "user_id_hash"] {
            if let Some(Value::String(hash)) = correlation.get_mut(field) {
                *hash = hash.to_ascii_lowercase();
            }
        }
    }
    let Some(payload) = event.get_mut("payload") else {
        return false;
    };
    if let Some(signal) = payload.get_mut("signal") {
        trim_fields(
            signal,
            &[
                "action_key",
                "funnel_key",
                "step_key",
                "conversion_key",
                "marker_key",
            ],
        );
    }
    for name in ["route", "previous_route"] {
        if let Some(route) = payload.get_mut(name) {
            trim_fields(route, &["path", "normalized_path", "title"]);
        }
    }
    if let Some(dimensions) = payload.get_mut("dimensions") {
        trim_fields(
            dimensions,
            &[
                "browser_family",
                "os_family",
                "language",
                "locale",
                "referrer_domain",
                "utm_source",
                "utm_medium",
                "utm_campaign",
                "country_code",
                "region_code",
            ],
        );
    }
    if let Some(dimensions) = payload
        .get_mut("custom_dimensions")
        .and_then(Value::as_object_mut)
    {
        let mut output = serde_json::Map::new();
        for (key, mut value) in std::mem::take(dimensions) {
            if let Value::String(text) = &mut value {
                *text = text.trim().to_owned();
            }
            if output.insert(key.trim().to_owned(), value).is_some() {
                return false;
            }
        }
        *dimensions = output;
    }
    true
}
fn reserved_key(value: &str) -> bool {
    let mut key = String::new();
    let mut separator = false;
    for character in value.to_lowercase().chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !key.is_empty() {
                key.push('_');
            }
            key.push(character);
            separator = false;
        } else {
            separator = true;
        }
    }
    [
        "auth_state",
        "device_type",
        "browser_family",
        "browser_major",
        "os_family",
        "os_major",
        "language",
        "locale",
        "viewport_bucket",
        "referrer_domain",
        "utm_source",
        "utm_medium",
        "utm_campaign",
        "country_code",
        "region_code",
        "email",
        "email_address",
        "phone",
        "phone_number",
        "password",
        "passcode",
        "token",
        "secret",
        "api_key",
        "apikey",
        "authorization",
        "auth_token",
        "card",
        "card_number",
        "credit_card",
        "ssn",
        "ip",
        "ip_address",
        "user_id",
        "userid",
        "account_id",
        "workspace_id",
        "order_id",
        "ticket_id",
        "url",
        "href",
        "query",
        "text",
        "message",
        "body",
        "payload",
        "session_id",
        "visitor_id",
    ]
    .contains(&key.as_str())
        || ["_id", "_token", "_secret", "_password"]
            .iter()
            .any(|suffix| key.ends_with(suffix))
}

#[cfg(any(feature = "local", feature = "testkit"))]
pub(crate) fn protect_event(privacy: &crate::privacy::Privacy, event: &Value) -> Option<Value> {
    if !valid_event(event) {
        return None;
    }
    let mut output = event.clone();
    for key in ["service", "payload"] {
        output[key] = privacy.protect(&output[key])?;
    }
    if output["correlation"]
        .as_object()?
        .values()
        .filter_map(Value::as_str)
        .any(|value| privacy.text(value) != value)
    {
        return None;
    }
    for key in [
        "schema_version",
        "event_id",
        "sdk_name",
        "sdk_version",
        "occurred_at",
        "project_id",
    ] {
        if output[key]
            .as_str()
            .is_some_and(|value| privacy.text(value) != value)
        {
            return None;
        }
    }
    output.as_object_mut()?.remove("project_token");
    valid_event(&output).then_some(output)
}
