use debugbundle::wire::{canonical_size, capabilities, valid_event};
use serde_json::Value;

#[test]
fn canonical_wire_and_semantic_corpus() {
    let corpus: Value =
        serde_json::from_str(include_str!("fixtures/evidence-wire-cases.json")).unwrap();
    for case in corpus["events"].as_array().unwrap() {
        assert_eq!(
            valid_event(&case["event"]),
            case["valid"].as_bool().unwrap(),
            "{}",
            case["id"]
        );
    }
    for case in corpus["capabilities"].as_array().unwrap() {
        assert_eq!(
            serde_json::to_value(capabilities(&case["input"])).unwrap(),
            case["features"],
            "{}",
            case["id"]
        );
    }
    for case in corpus["json_sizes"].as_array().unwrap() {
        assert_eq!(
            canonical_size(&case["value"]),
            case["bytes"].as_u64().map(|v| v as usize),
            "{}",
            case["value"]
        );
    }
}

#[test]
fn graph_cycles_and_utf8_limits_cannot_be_hidden_by_json_schema() {
    let corpus: Value =
        serde_json::from_str(include_str!("fixtures/evidence-wire-cases.json")).unwrap();
    let mut event = corpus["events"][0]["event"].clone();
    event["payload"]["exception_evidence"]["nodes"][0]["message"] = Value::String("é".repeat(1025));
    assert!(!valid_event(&event));
    event = corpus["events"][0]["event"].clone();
    event["payload"]["exception_evidence"]["nodes"][0]["relationships"] =
        serde_json::json!([{"kind":"cause","target_id":0}]);
    assert!(!valid_event(&event));
}

#[test]
fn evidence_integer_fields_follow_json_number_semantics() {
    let corpus: Value =
        serde_json::from_str(include_str!("fixtures/evidence-wire-cases.json")).unwrap();
    let mut event = corpus["events"][0]["event"].clone();
    let graph = &mut event["payload"]["exception_evidence"];
    graph["version"] = serde_json::json!(1.0);
    graph["root_id"] = serde_json::json!(0.0);
    graph["nodes"][0]["id"] = serde_json::json!(0.0);
    assert!(valid_event(&event));
    assert_eq!(
        capabilities(&serde_json::json!({"version":1.0,"features":["exception_evidence_v1"]})),
        vec!["exception_evidence_v1"]
    );
    assert!(debugbundle::wire::valid_reference(
        &serde_json::json!({"version":1.0,"failure_ref":"11111111-1111-4111-8111-111111111111","scope":{"kind":"page","page_id":"22222222-2222-4222-8222-222222222222"}})
    ));
}
