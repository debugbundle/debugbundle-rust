use debugbundle::privacy::Privacy;
use serde_json::{json, Value};

#[test]
fn shared_privacy_corpus_is_preserved_exactly() {
    let corpus: Value =
        serde_json::from_str(include_str!("fixtures/privacy-conformance.json")).unwrap();
    let privacy = Privacy::new(&[]).unwrap();
    for case in corpus["cases"].as_array().unwrap() {
        assert_eq!(
            privacy.protect(&case["input"]),
            Some(case["expected"].clone()),
            "{}",
            case["id"]
        );
    }
}

#[test]
fn custom_fields_extend_mandatory_rules_and_keys_are_segment_aware() {
    let privacy = Privacy::new(&["tenant_pin".into()]).unwrap();
    assert_eq!(
        privacy.protect(
            &json!({"password":"x", "tenant_pin":"y", "tokenizer":"safe", "myAccessToken":42})
        ),
        Some(
            json!({"password":"[REDACTED]", "tenant_pin":"[REDACTED]", "tokenizer":"safe", "myAccessToken":"[REDACTED]"})
        )
    );
}

#[test]
fn malformed_encoded_credentials_stay_private_when_a_later_pass_withholds_more_context() {
    let privacy = Privacy::new(&[]).unwrap();
    let input = json!({"body":"{signal\"password\":\"SYNTHETIC_SECRET\",\"route\":\"/checkout\"}"});
    let first = privacy.protect(&input).unwrap();
    assert_eq!(
        first,
        json!({"body":"{signal\"password\":[REDACTED],\"route\":\"/checkout\"}"})
    );
    assert!(!serde_json::to_string(&first)
        .unwrap()
        .contains("SYNTHETIC_SECRET"));
    // This is the shared sanitizer's established malformed-structured fallback,
    // rather than a promise that arbitrary encoded text has one-pass equality.
    assert_eq!(privacy.protect(&first), Some(json!({"body":"[REDACTED]"})));
}

#[test]
fn budgets_withhold_oversized_names_and_values() {
    let privacy = Privacy::new(&[]).unwrap();
    assert_eq!(
        privacy.protect(&json!({"message":"x".repeat(17000)})),
        Some(json!({"message":"[REDACTED]"}))
    );
    assert_eq!(
        privacy.protect(&json!({"k".repeat(129):"sensitive"})),
        Some(json!({}))
    );
    assert!(Privacy::new(&["".into()]).is_none());
}

#[test]
fn unified_arbitrary_precision_numbers_cannot_escape_native_value_budgets() {
    let privacy = Privacy::new(&[]).unwrap();
    let number: Value = serde_json::from_str(&format!("1{}", "0".repeat(100_000))).unwrap();
    assert!(privacy.protect(&number).is_none());
    assert!(debugbundle::wire::canonical_size(&number).is_none());
    assert_eq!(privacy.protect(&json!(12345)), Some(json!(12345)));
}
