use super::RelayOptions;
use crate::privacy::Privacy;
use serde_json::{json, Value};
use std::sync::LazyLock;
static DEBUG_SCHEMA: LazyLock<Option<Value>> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/event-envelope.schema.json"
    ))
    .ok()
});
static ANALYTICS_SCHEMA: LazyLock<Option<Value>> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/analytics-envelope.schema.json"
    ))
    .ok()
});
pub(super) fn event(
    input: &Value,
    privacy: &Privacy,
    options: &RelayOptions,
    features: &[String],
) -> Option<Value> {
    if !input.is_object() || !crate::wire::preflight(input) {
        return None;
    }
    let kind = input["event_type"].as_str()?;
    if ![
        "frontend_exception",
        "frontend_breadcrumb",
        "request_event",
        "probe_event",
        "error_suppressed",
        "diagnostic_context",
        "analytics_event",
    ]
    .contains(&kind)
    {
        return None;
    }
    let analytics = kind == "analytics_event";
    let schema = if analytics {
        ANALYTICS_SCHEMA.as_ref()?
    } else {
        DEBUG_SCHEMA.as_ref()?
    };
    let mut output = project(schema, input)?;
    output["sdk_name"] = json!("@debugbundle/sdk-browser");
    output.as_object_mut()?.remove("project_token");
    output.as_object_mut()?.remove("project_id");
    if let Some(service) = &options.service {
        output["service"]
            .as_object_mut()?
            .insert("name".into(), json!(service));
    }
    if let Some(environment) = &options.environment {
        output["service"]
            .as_object_mut()?
            .insert("environment".into(), json!(environment));
    }
    if analytics {
        return crate::analytics_wire::protect_event(privacy, &output);
    }
    // Typed evidence is closed and validated before any negotiated downgrade.
    if !crate::wire::valid_event(&output) {
        return None;
    }
    if kind == "diagnostic_context" && output["payload"]["scope"]["kind"] != "page" {
        return None;
    }
    if output["payload"].get("execution").is_some() {
        return None;
    }
    if crate::wire::valid_reference(&output["context"]["evidence_reference"])
        && output["context"]["evidence_reference"]["scope"]["kind"] != "page"
    {
        return None;
    }
    for_delivery(&output, privacy, features)
}
pub(crate) fn for_delivery(input: &Value, privacy: &Privacy, features: &[String]) -> Option<Value> {
    if !crate::wire::preflight(input) {
        return None;
    }
    if input["event_type"] == "analytics_event" {
        return crate::analytics_wire::protect_event(privacy, input);
    }
    let mut output = input.clone();
    let kind = output["event_type"].as_str()?;
    let supported = |feature: &str| features.iter().any(|value| value == feature);
    if kind == "diagnostic_context"
        && !supported(if output["payload"]["kind"] == "recovery" {
            "recovery_v1"
        } else {
            "resource_observations_v1"
        })
    {
        return None;
    }
    if !supported("exception_evidence_v1") {
        output["payload"]
            .as_object_mut()?
            .remove("exception_evidence");
    }
    if !supported("resource_observations_v1") {
        output["payload"]
            .as_object_mut()?
            .remove("resource_observations");
    }
    if !supported("evidence_provenance_v1") {
        if let Some(context) = output.get_mut("context").and_then(Value::as_object_mut) {
            context.remove("evidence_reference");
        }
    }
    crate::wire::protect_event(privacy, &output)
        .and_then(|event| crate::wire::fit_event(&event, crate::wire::MAX_EVENT_BYTES))
}
fn matches(schema: &Value, value: &Value) -> bool {
    if let Some(constant) = schema.get("const") {
        return value == constant;
    }
    let typed = match schema["type"].as_str() {
        Some("object") => value.is_object(),
        Some("array") => value.is_array(),
        Some("string") => value.is_string(),
        Some("null") => value.is_null(),
        Some("boolean") => value.is_boolean(),
        Some("integer" | "number") => value.is_number(),
        _ => true,
    };
    typed
        && schema["properties"].as_object().is_none_or(|properties| {
            properties.iter().all(|(key, schema)| {
                schema
                    .get("const")
                    .is_none_or(|constant| value.get(key) == Some(constant))
            })
        })
}
fn project(schema: &Value, value: &Value) -> Option<Value> {
    if let Some(branches) = schema["anyOf"].as_array() {
        return project(
            branches.iter().find(|schema| matches(schema, value))?,
            value,
        );
    }
    match value {
        Value::Object(map) => {
            let properties = schema["properties"].as_object();
            let mut output = serde_json::Map::new();
            for (key, value) in map {
                if let Some(property) = properties.and_then(|properties| properties.get(key)) {
                    let value = if [
                        "exception_evidence",
                        "execution",
                        "resource_observations",
                        "evidence_reference",
                        "scope",
                    ]
                    .contains(&key.as_str())
                    {
                        value.clone()
                    } else {
                        project(property, value)?
                    };
                    output.insert(key.clone(), value);
                } else if schema["additionalProperties"] != false {
                    output.insert(
                        key.clone(),
                        if schema["additionalProperties"].is_object() {
                            project(&schema["additionalProperties"], value)?
                        } else {
                            value.clone()
                        },
                    );
                }
            }
            Some(Value::Object(output))
        }
        Value::Array(values) => Some(Value::Array(
            values
                .iter()
                .map(|value| project(&schema["items"], value))
                .collect::<Option<Vec<_>>>()?,
        )),
        _ => Some(value.clone()),
    }
}
