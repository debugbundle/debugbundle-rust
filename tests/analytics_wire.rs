use serde_json::Value;
#[test]
fn analytics_uses_its_own_receiving_schema_and_semantic_contract() {
    let corpus: Value =
        serde_json::from_str(include_str!("fixtures/analytics-wire-cases.json")).unwrap();
    for case in corpus["events"].as_array().unwrap() {
        assert_eq!(
            debugbundle::wire::valid_analytics_event(&case["event"]),
            case["valid"].as_bool().unwrap(),
            "{}",
            case["id"]
        );
    }
    assert!(!debugbundle::wire::valid_event(
        &corpus["events"][0]["event"]
    ));
}

#[test]
fn malformed_native_json_is_rejected_without_unwinding() {
    for value in [
        Value::Null,
        serde_json::json!(true),
        serde_json::json!(7),
        serde_json::json!([]),
        serde_json::json!({"service":false,"payload":[]}),
    ] {
        assert!(!debugbundle::wire::valid_analytics_event(&value));
    }
}
