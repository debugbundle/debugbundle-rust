use debugbundle::{privacy::Privacy, wire};
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
    let exact_cases = corpus["exact_key_cases"].as_array().unwrap();
    assert!(!exact_cases.is_empty());
    for case in exact_cases {
        let additional = case
            .get("additional_keys")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| item.as_str().unwrap().to_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let exact = case
            .get("exact_keys")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| item.as_str().unwrap().to_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let exact_privacy = Privacy::with_exact_fields(&additional, &exact).unwrap();
        let protected = exact_privacy.protect(&case["input"]).unwrap();
        assert_eq!(protected, case["expected"], "{}", case["id"]);
        assert_eq!(
            exact_privacy.protect(&protected),
            Some(protected),
            "{} idempotent",
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
fn public_key_matcher_rejects_oversized_names_before_segment_work() {
    let privacy = Privacy::new(&[]).unwrap();
    assert!(!privacy.sensitive_key(&"x".repeat(128)));
    assert!(privacy.sensitive_key(&"x".repeat(129)));
    assert!(privacy.sensitive_key(&"x_".repeat(100_000)));
}

#[test]
fn unified_arbitrary_precision_numbers_cannot_escape_native_value_budgets() {
    let privacy = Privacy::new(&[]).unwrap();
    let number: Value = serde_json::from_str(&format!("1{}", "0".repeat(100_000))).unwrap();
    assert!(privacy.protect(&number).is_none());
    assert!(debugbundle::wire::canonical_size(&number).is_none());
    assert_eq!(privacy.protect(&json!(12345)), Some(json!(12345)));
}

#[test]
fn exact_fields_enforce_combined_bounds_literal_matching_and_ascii_boundaries() {
    let half_broad: Vec<String> = (0..64).map(|i| format!("broad_{i}")).collect();
    let half_exact: Vec<String> = (0..64).map(|i| format!("exact_{i}")).collect();
    assert!(Privacy::with_exact_fields(&half_broad, &half_exact).is_some());

    let mut over_exact = half_exact.clone();
    over_exact.push("exact_64".into());
    assert!(Privacy::with_exact_fields(&half_broad, &over_exact).is_none());

    assert!(Privacy::with_exact_fields(&[], &["".into()]).is_none());
    assert!(Privacy::with_exact_fields(&[], &["   ".into()]).is_none());
    assert!(Privacy::with_exact_fields(&[], &["x".repeat(65)]).is_none());
    assert!(Privacy::with_exact_fields(&[], &["x".repeat(64)]).is_some());

    // Regex metacharacters in exact keys stay literal (no user regex execution).
    let literal = Privacy::with_exact_fields(&[], &["foo.bar".into(), "a+b".into()]).unwrap();
    assert_eq!(
        literal.protect(&json!({
            "foo.bar": "SYNTHETIC_DOT",
            "FOO.BAR": "SYNTHETIC_DOT_UPPER",
            "fooXbar": "preserve-non-literal",
            "a+b": "SYNTHETIC_PLUS",
            "aaab": "preserve-non-quantified",
            "text": "foo.bar=SYNTHETIC_TEXT fooXbar=keep a+b=SYNTHETIC_PLUS_TEXT aaab=keep"
        })),
        Some(json!({
            "foo.bar": "[REDACTED]",
            "FOO.BAR": "[REDACTED]",
            "fooXbar": "preserve-non-literal",
            "a+b": "[REDACTED]",
            "aaab": "preserve-non-quantified",
            "text": "foo.bar=[REDACTED] fooXbar=keep a+b=[REDACTED] aaab=keep"
        }))
    );

    // Exact assignment scrubbing checks ASCII identifier boundaries cleanly.
    let exact = Privacy::with_exact_fields(&[], &[" tag ".into(), "k".into()]).unwrap();
    assert!(exact.sensitive_key("tag"));
    assert!(exact.sensitive_key("TAG"));
    assert!(!exact.sensitive_key("tag_name"));
    assert!(exact.sensitive_key(&"x".repeat(129)));
    assert_eq!(
        exact.text(
            "TAG: \"quoted secret\" tag='single secret' my-tag=tag=SYNTHETIC_INNER tag_name=script tag-name=img tagName=link \u{212A}=keep tag="
        ),
        "TAG: [REDACTED] tag=[REDACTED] my-tag=tag=[REDACTED] tag_name=script tag-name=img tagName=link \u{212A}=keep tag="
    );
}

#[test]
fn protocol_preservation_shields_closed_structures_and_fails_closed_on_invalid_shapes() {
    let broad = Privacy::new(&[
        "tag".into(),
        "reference".into(),
        "reason".into(),
        "file".into(),
    ])
    .unwrap();

    // Queue pressure reason survives broad `reason` redaction.
    let suppressed = json!({
        "schema_version": "2026-03-01",
        "event_id": "11111111-1111-4111-8111-111111111111",
        "event_type": "error_suppressed",
        "sdk_name": "@debugbundle/sdk-rust",
        "sdk_version": "1.0.2",
        "occurred_at": "2026-11-25T10:00:00.000Z",
        "service": {"name": "billing", "environment": "production", "runtime": "rust"},
        "correlation": {"request_id": null, "trace_id": null, "session_id": null, "user_id_hash": null},
        "context": {
            "probe_limits": {"version": 1, "reason": "budget_exceeded"},
            "evidence_limits": {"version": 1, "exception_evidence": "budget_exceeded"},
            "payout": {"reason": "SYNTHETIC_REASON"}
        },
        "payload": {
            "fingerprint": "sdk-queue-pressure",
            "suppressed_count": 3,
            "window_seconds": 30,
            "first_seen": "2026-11-25T09:59:30.000Z",
            "last_seen": "2026-11-25T10:00:00.000Z",
            "reason": "queue_pressure",
            "level": "error"
        }
    });
    let protected = wire::protect_event(&broad, &suppressed).unwrap();
    assert_eq!(protected["payload"]["reason"], "queue_pressure");
    assert_eq!(
        protected["context"]["probe_limits"],
        json!({"version": 1, "reason": "budget_exceeded"})
    );
    assert_eq!(
        protected["context"]["evidence_limits"],
        json!({"version": 1, "exception_evidence": "budget_exceeded"})
    );
    assert_eq!(protected["context"]["payout"]["reason"], "[REDACTED]");
    assert_eq!(wire::protect_event(&broad, &protected), Some(protected));

    // All 10 closed HTML resource tag names survive broad `tag`, while arbitrary
    // tags and forged evidence_reference/evidence_limits are never exempted.
    let base_frontend = |tag_name: &str| {
        json!({
            "schema_version": "2026-03-01",
            "event_id": "11111111-1111-4111-8111-111111111112",
            "event_type": "frontend_exception",
            "sdk_name": "@debugbundle/sdk-browser",
            "sdk_version": "3.2.2",
            "occurred_at": "2026-11-25T10:00:00.000Z",
            "service": {"name": "web", "environment": "production", "runtime": "browser"},
            "correlation": {"request_id": null, "trace_id": null, "session_id": null, "user_id_hash": null},
            "context": {
                "evidence_reference": {"reference": "SYNTHETIC_FORGED_REF"},
                "evidence_limits": {"version": 1, "reason": "SYNTHETIC_FORGED_LIMIT"}
            },
            "payload": {
                "name": "Error",
                "message": "resource failed",
                "stack": "Error: resource failed",
                "browser": {"name": "Chrome", "version": "135.0.0"},
                "browser_event": {
                    "kind": "resource_error",
                    "message": null,
                    "file_name": null,
                    "line_number": null,
                    "column_number": null,
                    "target": {"tag_name": tag_name, "source_url": null},
                    "opaque": true
                }
            }
        })
    };
    for allowed in [
        "script", "LINK", "img", "audio", "video", "source", "iframe", "object", "embed", "input",
    ] {
        let protected = wire::protect_event(&broad, &base_frontend(allowed)).unwrap();
        assert_eq!(
            protected["payload"]["browser_event"]["target"]["tag_name"],
            allowed
        );
        // Invalid evidence_reference / evidence_limits fell back to broad redaction.
        assert_eq!(protected["context"]["evidence_reference"], "[REDACTED]");
        assert_eq!(
            protected["context"]["evidence_limits"]["reason"],
            "[REDACTED]"
        );
    }
    let non_allowed = wire::protect_event(&broad, &base_frontend("div")).unwrap();
    assert_eq!(
        non_allowed["payload"]["browser_event"]["target"]["tag_name"],
        "[REDACTED]"
    );

    // Valid optional evidence that expands past a field limit during scrubbing
    // yields to the primary event instead of dropping the exception.
    let mut expanding = json!({
        "schema_version": "2026-03-01",
        "event_id": "11111111-1111-4111-8111-111111111113",
        "event_type": "backend_exception",
        "sdk_name": "@debugbundle/sdk-rust",
        "sdk_version": "1.0.2",
        "occurred_at": "2026-11-25T10:00:00.000Z",
        "service": {"name": "billing", "environment": "production", "runtime": "rust"},
        "correlation": {"request_id": null, "trace_id": null, "session_id": null, "user_id_hash": null},
        "context": {},
        "payload": {
            "name": "Error",
            "message": "primary stays intact",
            "stack": "Stack unavailable",
            "handled": true,
            "request": {"method": "GET", "path": "/", "query": {}, "headers": {}},
            "response": {"status_code": 500},
            "runtime": {"version": "1.89"},
            "execution": {
                "version": 1,
                "kind": "job",
                "name": format!("{} tag=x", "a".repeat(122)),
                "elapsed_ms": 12.5,
                "limit_ms": 100.0
            }
        }
    });
    let reduced = wire::protect_event(&broad, &expanding).unwrap();
    assert_eq!(reduced["payload"]["message"], "primary stays intact");
    assert!(reduced["payload"].get("execution").is_none());
    assert_eq!(
        reduced["context"]["evidence_limits"]["execution"],
        "budget_exceeded"
    );

    // Malformed diagnostic_context payload fails closed.
    expanding["event_type"] = json!("diagnostic_context");
    expanding["payload"] = json!({"version": 1, "kind": "unknown_kind"});
    assert!(wire::protect_event(&broad, &expanding).is_none());
}
