#![cfg(feature = "local")]
use debugbundle::{
    relay::{Relay, RelayOptions, RelayRequest},
    Client, Config,
};
use serde_json::{json, Value};
use std::{net::IpAddr, time::Duration};

#[tokio::test]
async fn downstream_revocation_is_applied_to_every_unsent_relay_chunk() {
    use debugbundle::delivery::{Transport, TransportFailure, TransportResponse};
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct Destination(Mutex<Vec<Vec<Value>>>);
    impl Transport for Destination {
        fn send(&self, body: &[u8], _: Duration) -> Result<TransportResponse, TransportFailure> {
            let body: Value = serde_json::from_slice(body).unwrap();
            let events = body["events"].as_array().unwrap().clone();
            let count = events.len();
            self.0.lock().unwrap().push(events);
            Ok(TransportResponse {
                status: 202,
                body: Some(
                    json!({"accepted":count,"rejected":0,"errors":[],"evidence_capabilities":{"version":1,"features":[]}}),
                ),
                retry_after: None,
            })
        }
    }
    let destination = Arc::new(Destination::default());
    let client = Client::new(Config {
        transport: Some(destination.clone()),
        evidence_capabilities: Some(json!({"version":1,"features":["exception_evidence_v1"]})),
        ..Config::new("synthetic", "checkout")
    });
    let relay = Relay::new(
        client,
        RelayOptions {
            durable_write: false,
            ..RelayOptions::default()
        },
    );
    let evidence = debugbundle::ExceptionSnapshot::new("SyntheticFailure", "safe fixture");
    let mut events = (0..20)
        .map(|_| {
            let mut event = browser_event();
            event["event_id"] = json!(uuid::Uuid::new_v4().to_string());
            event["sdk_name"] = json!("x");
            event["payload"]["exception_evidence"] = evidence.evidence().clone();
            event
        })
        .collect::<Vec<_>>();
    // Input fits one browser request; the forced canonical SDK identity makes
    // the protected upstream wrapper require another actual-byte chunk.
    let (mut low, mut high) = (0usize, 14000usize);
    while low < high {
        let middle = (low + high).div_ceil(2);
        for event in &mut events {
            event["payload"]["stack"] = json!("x".repeat(middle));
        }
        let size = serde_json::to_vec(&json!({"batch":events})).unwrap().len();
        if size <= debugbundle::wire::MAX_BODY_BYTES - 32 {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    for event in &mut events {
        event["payload"]["stack"] = json!("x".repeat(low));
    }
    let body = serde_json::to_vec(&json!({"batch":events})).unwrap();
    let mut head = request();
    head.evidence = true;
    let response = relay.handle(&head, &body).await;
    assert_eq!(response.status, 202);
    let chunks = destination.0.lock().unwrap();
    assert!(chunks.len() > 1);
    assert!(chunks[0][0]["payload"].get("exception_evidence").is_some());
    for chunk in &chunks[1..] {
        for event in chunk {
            assert!(event["payload"].get("exception_evidence").is_none());
        }
    }
    assert_eq!(
        response.body.unwrap()["evidence_capabilities"],
        json!({"version":1,"features":[]})
    );
}
fn request() -> RelayRequest {
    RelayRequest {
        method: "POST".into(),
        scheme: "https".into(),
        host: "app.example.com".into(),
        origin: Some("https://app.example.com".into()),
        content_type: Some("application/json; charset=utf-8".into()),
        peer: Some("203.0.113.10".parse::<IpAddr>().unwrap()),
        ..RelayRequest::default()
    }
}
fn browser_event() -> Value {
    let fixtures: Value =
        serde_json::from_str(include_str!("fixtures/relay-compliance.json")).unwrap();
    fixtures["cases"][0]["request"]["bodyJson"]["batch"][0].clone()
}

#[tokio::test]
async fn relay_owner_hook_is_private_authoritative_and_runs_before_local_retention() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let directory = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let config = Config {
        before_send: Some(Arc::new(move |mut event| {
            count.fetch_add(1, Ordering::SeqCst);
            assert!(!event.to_string().contains("SYNTHETIC_SECRET"));
            event["event_id"] = json!("00000000-0000-4000-8000-000000000999");
            event["sdk_name"] = json!("owner-edited-sdk");
            event["payload"]["message"] = json!("owner changed password=SYNTHETIC_SECRET");
            Some(event)
        })),
        ..Config::local("checkout", directory.path().join("events"))
    };
    let relay = Relay::new(Client::new(config), RelayOptions::default());
    let mut event = browser_event();
    event["payload"]["message"] = json!("password=SYNTHETIC_SECRET");
    assert_eq!(
        relay
            .handle(
                &request(),
                &serde_json::to_vec(&json!({"batch":[event]})).unwrap()
            )
            .await
            .status,
        202
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let file = std::fs::read_dir(directory.path().join("events"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let body = std::fs::read(file.path()).unwrap();
    let events: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        events[0]["event_id"],
        "00000000-0000-4000-8000-000000000999"
    );
    assert_eq!(events[0]["sdk_name"], "owner-edited-sdk");
    assert!(!String::from_utf8(body)
        .unwrap()
        .contains("SYNTHETIC_SECRET"));
}

#[tokio::test]
async fn relay_hook_drop_has_no_persistence_or_false_acceptance() {
    let directory = tempfile::tempdir().unwrap();
    let relay = Relay::new(
        Client::new(Config {
            before_send: Some(std::sync::Arc::new(|_| None)),
            ..Config::local("checkout", directory.path().join("events"))
        }),
        RelayOptions::default(),
    );
    let mut head = request();
    head.evidence = true;
    let response = relay
        .handle(
            &head,
            &serde_json::to_vec(&json!({"batch":[browser_event()]})).unwrap(),
        )
        .await;
    assert_eq!(response.status, 202);
    let body = response.body.unwrap();
    assert_eq!(body["accepted"], 0);
    assert_eq!(body["rejected"], 1);
    assert_eq!(body["evidence_errors"][0]["reason"], "local_policy");
    assert!(!directory.path().join("events").exists());
}
#[tokio::test]
async fn local_relay_accepts_only_canonical_analytics_and_strips_browser_credentials() {
    let fixtures: Value =
        serde_json::from_str(include_str!("fixtures/relay-compliance.json")).unwrap();
    let case = fixtures["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "valid-analytics-event")
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let client = Client::new(Config::local("checkout", directory.path().join("events")));
    let relay = Relay::new(client, RelayOptions::default());
    let body = serde_json::to_vec(&case["request"]["bodyJson"]).unwrap();
    let response = relay.handle(&request(), &body).await;
    assert_eq!(response.status, 202);
    assert_eq!(response.body.as_ref().unwrap()["accepted"], 1);
    let file = std::fs::read_dir(directory.path().join("events"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let events: Value = serde_json::from_slice(&std::fs::read(file.path()).unwrap()).unwrap();
    assert!(debugbundle::wire::valid_analytics_event(&events[0]));
    assert_eq!(events[0]["sdk_name"], "@debugbundle/sdk-browser");
    assert!(events[0].get("project_token").is_none());
}
#[tokio::test]
async fn local_relay_preserves_browser_identity_and_never_accepts_backend_impersonation() {
    let directory = tempfile::tempdir().unwrap();
    let client = Client::new(Config::local("checkout", directory.path().join("events")));
    let relay = Relay::new(client.clone(), RelayOptions::default());
    let original = browser_event();
    let mut forged = original.clone();
    forged["event_type"] = json!("backend_exception");
    let body = serde_json::to_vec(&json!({"batch":[original,forged]})).unwrap();
    let mut head = request();
    head.evidence = true;
    let response = relay.handle(&head, &body).await;
    assert_eq!(response.status, 202);
    assert_eq!(response.body.as_ref().unwrap()["accepted"], 1);
    assert_eq!(response.body.as_ref().unwrap()["rejected"], 1);
    assert_eq!(
        response.body.as_ref().unwrap()["evidence_errors"][0]["index"],
        1
    );
    let files = std::fs::read_dir(directory.path().join("events"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(files.len(), 1);
    let events: Value = serde_json::from_slice(&std::fs::read(files[0].path()).unwrap()).unwrap();
    assert_eq!(events[0]["sdk_name"], "@debugbundle/sdk-browser");
    assert_eq!(
        events[0]["correlation"]["trace_id"],
        "11111111-1111-4111-8111-111111111111"
    );
    assert_eq!(events[0]["service"]["name"], "checkout-web");
    assert!(events[0].get("project_token").is_none());
    assert!(client.shutdown_async(Duration::from_secs(1)).await.drained);
}
#[tokio::test]
async fn relay_origin_cors_content_type_and_capacity_are_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let client = Client::new(Config::local("checkout", directory.path().join("events")));
    let relay = Relay::new(
        client,
        RelayOptions {
            rate_limit_per_minute: 1,
            ..RelayOptions::default()
        },
    );
    let mut head = request();
    let body = serde_json::to_vec(&json!({"batch":[browser_event()]})).unwrap();
    head.origin = Some("https://evil.example".into());
    assert_eq!(relay.handle(&head, &body).await.status, 403);
    head = request();
    head.method = "OPTIONS".into();
    let response = relay.handle(&head, &[]).await;
    assert_eq!(response.status, 204);
    assert_eq!(
        response.headers.get("access-control-allow-origin").unwrap(),
        "https://app.example.com"
    );
    head = request();
    head.content_type = Some("text/plain".into());
    assert_eq!(relay.handle(&head, &body).await.status, 400);
    head = request();
    assert_eq!(
        relay
            .handle(&head, &vec![b'x'; 256 * 1024 + 1])
            .await
            .status,
        413
    );
    assert_eq!(relay.handle(&head, &body).await.status, 202);
    assert_eq!(relay.handle(&head, &body).await.status, 429);
}

#[tokio::test]
async fn relay_preserves_browser_protocol_structures_under_broad_and_exact_redaction() {
    let failure_ref = "11111111-1111-4111-8111-111111111111";
    let page_id = "22222222-2222-4222-8222-222222222222";
    let source_event_id = "33333333-3333-4333-8333-333333333333";

    let build_frontend = |id: &str, tag_name: &str| {
        let mut event = browser_event();
        event["event_id"] = json!(id);
        event["correlation"]["session_id"] = json!("44444444-4444-4444-8444-444444444444");
        event["context"] = json!({
            "evidence_reference": {
                "version": 1,
                "failure_ref": failure_ref,
                "scope": {"kind": "page", "page_id": page_id}
            },
            "evidence_limits": {
                "version": 1,
                "exception_evidence": "unsupported"
            },
            "payout": {
                "tag": "SYNTHETIC_TAG",
                "tag_name": "script",
                "reference": "SYNTHETIC_REF",
                "reason": "SYNTHETIC_REASON",
                "file": "SYNTHETIC_FILE"
            }
        });
        event["payload"]["message"] = json!("chunk failed tag=SYNTHETIC_MSG_TAG tag_name=script");
        event["payload"]["browser_event"] = json!({
            "kind": "resource_error",
            "message": "load failed tag=SYNTHETIC_BE_MSG",
            "file_name": "https://cdn.example.test/app.js?token=SYNTHETIC_FN_TOKEN&file=SYNTHETIC_FN_FILE",
            "line_number": 12,
            "column_number": 4,
            "target": {
                "tag_name": tag_name,
                "source_url": "https://cdn.example.test/bundle.js?tag=SYNTHETIC_SRC_TAG&tag_name=script&safe=1",
                "attributes": {
                    "rel": "preload",
                    "as": "script",
                    "type": "text/javascript",
                    "media": "all",
                    "cross_origin": "anonymous",
                    "async": true,
                    "defer": false,
                    "integrity_present": true
                }
            },
            "page": {
                "url": "https://app.example.com/pay?reference=SYNTHETIC_PAGE_REF&ok=1",
                "referrer": "https://app.example.com/cart?reason=SYNTHETIC_REFERRER_REASON",
                "ready_state": "complete",
                "visibility_state": "visible"
            },
            "opaque": false
        });
        event["payload"]["rejection_reason"] = json!({
            "kind": "error",
            "name": "ChunkLoadError",
            "message": "rejected reason=SYNTHETIC_REJ_REASON",
            "preview": "{\"tag\":\"SYNTHETIC_PREVIEW_TAG\",\"tag_name\":\"script\"}"
        });
        event["payload"]["resource_observations"] = json!({
            "version": 1,
            "items": [
                {
                    "kind": "timing",
                    "observed_at": "2026-11-25T10:00:00.000Z",
                    "source_url": "https://cdn.example.test/bundle.js",
                    "availability": "observed",
                    "response_status": 502,
                    "duration_ms": 42.5,
                    "transfer_size_bytes": 1024
                },
                {
                    "kind": "csp",
                    "observed_at": "2026-11-25T10:00:00.000Z",
                    "source_url": "https://cdn.example.test/bundle.js",
                    "effective_directive": "script-src",
                    "disposition": "enforce",
                    "document_status_code": 200
                }
            ]
        });
        event["payload"]["exception_evidence"] = json!({
            "version": 1,
            "runtime": "browser",
            "root_id": 0,
            "nodes": [{
                "id": 0,
                "name": "ChunkLoadError",
                "message": "graph reason=SYNTHETIC_GRAPH_REASON",
                "stack": "ChunkLoadError\n at https://cdn.example.test/app.js?token=SYNTHETIC_STACK_TOKEN:10:2",
                "trace_origin": "throw",
                "parsing_status": "parsed",
                "frames": [{
                    "function": "loadChunk",
                    "file": "https://cdn.example.test/chunk.js",
                    "line": 10,
                    "column": 2,
                    "classification": "application"
                }],
                "relationships": []
            }],
            "truncated": false,
            "unavailable_reason": null
        });
        event
    };

    let diagnostic_resource = json!({
        "schema_version": "2026-03-01",
        "event_id": "55555555-5555-4555-8555-555555555555",
        "event_type": "diagnostic_context",
        "sdk_name": "@debugbundle/sdk-browser",
        "sdk_version": "3.2.2",
        "occurred_at": "2026-11-25T10:00:01.000Z",
        "service": {"name": "checkout-web", "runtime": "browser", "framework": "react", "environment": "production"},
        "correlation": {"request_id": null, "trace_id": "11111111-1111-4111-8111-111111111111", "session_id": "44444444-4444-4444-8444-444444444444", "user_id_hash": null},
        "context": {"payout": {"reference": "SYNTHETIC_DIAG_REF"}},
        "payload": {
            "version": 1,
            "kind": "resource_observation",
            "failure_ref": failure_ref,
            "scope": {"kind": "page", "page_id": page_id},
            "source_event_id": source_event_id,
            "observations": {
                "version": 1,
                "items": [{
                    "kind": "timing",
                    "observed_at": "2026-11-25T10:00:01.000Z",
                    "source_url": "https://cdn.example.test/late.js",
                    "availability": "observed",
                    "response_status": 404,
                    "duration_ms": 18.0,
                    "transfer_size_bytes": 0
                }]
            }
        }
    });

    // Broad redact_fields protects application context while preserving validated protocol blocks.
    let broad_dir = tempfile::tempdir().unwrap();
    let mut broad_config = Config::local("checkout", broad_dir.path().join("events"))
        .with_redact_fields(["tag", "reference", "reason", "file"]);
    broad_config.evidence_capabilities = Some(json!({
        "version": 1,
        "features": debugbundle::wire::FEATURES
    }));
    broad_config.before_send = Some(std::sync::Arc::new(|mut event| {
        if event["event_type"] == "frontend_exception" {
            event["context"]["payout"]["file"] = json!("SYNTHETIC_HOOK_FILE");
        }
        Some(event)
    }));
    let broad_relay = Relay::new(Client::new(broad_config), RelayOptions::default());
    let mut head = request();
    head.evidence = true;
    let broad_body = serde_json::to_vec(&json!({
        "batch": [
            build_frontend(source_event_id, "script"),
            build_frontend("66666666-6666-4666-8666-666666666666", "div"),
            diagnostic_resource
        ]
    }))
    .unwrap();
    let broad_response = broad_relay.handle(&head, &broad_body).await;
    assert_eq!(broad_response.status, 202);
    assert_eq!(broad_response.body.as_ref().unwrap()["accepted"], 3);
    assert_eq!(broad_response.body.as_ref().unwrap()["rejected"], 0);

    let file = std::fs::read_dir(broad_dir.path().join("events"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let bytes = std::fs::read(file.path()).unwrap();
    assert!(!String::from_utf8(bytes.clone())
        .unwrap()
        .contains("SYNTHETIC_"));
    let events: Value = serde_json::from_slice(&bytes).unwrap();
    let first = &events[0];
    assert_eq!(
        first["context"]["evidence_reference"],
        json!({"version": 1, "failure_ref": failure_ref, "scope": {"kind": "page", "page_id": page_id}})
    );
    assert_eq!(
        first["context"]["evidence_limits"],
        json!({"version": 1, "exception_evidence": "unsupported"})
    );
    assert_eq!(
        first["context"]["payout"],
        json!({
            "tag": "[REDACTED]",
            "tag_name": "[REDACTED]",
            "reference": "[REDACTED]",
            "reason": "[REDACTED]",
            "file": "[REDACTED]"
        })
    );
    assert_eq!(
        first["payload"]["browser_event"]["target"]["tag_name"],
        "script"
    );
    assert_eq!(
        first["payload"]["browser_event"]["message"],
        "load failed tag=[REDACTED]"
    );
    assert_eq!(
        first["payload"]["browser_event"]["file_name"],
        "https://cdn.example.test/app.js?token=%5BREDACTED%5D&file=%5BREDACTED%5D"
    );
    assert_eq!(
        first["payload"]["browser_event"]["target"]["source_url"],
        "https://cdn.example.test/bundle.js?tag=%5BREDACTED%5D&tag_name=%5BREDACTED%5D&safe=1"
    );
    assert_eq!(
        first["payload"]["browser_event"]["page"]["url"],
        "https://app.example.com/pay?reference=%5BREDACTED%5D&ok=1"
    );
    assert_eq!(
        first["payload"]["browser_event"]["page"]["referrer"],
        "https://app.example.com/cart?reason=%5BREDACTED%5D"
    );
    assert_eq!(
        first["payload"]["browser_event"]["page"]["ready_state"],
        "complete"
    );
    assert_eq!(
        first["payload"]["browser_event"]["page"]["visibility_state"],
        "visible"
    );
    assert_eq!(first["payload"]["rejection_reason"]["kind"], "error");
    assert_eq!(
        first["payload"]["rejection_reason"]["message"],
        "rejected reason=[REDACTED]"
    );
    assert_eq!(
        first["payload"]["rejection_reason"]["preview"],
        "{\"tag\":\"[REDACTED]\",\"tag_name\":\"[REDACTED]\"}"
    );
    assert_eq!(
        first["payload"]["exception_evidence"]["nodes"][0]["frames"][0]["file"],
        "https://cdn.example.test/chunk.js"
    );
    assert_eq!(
        first["payload"]["resource_observations"]["items"][0]["availability"],
        "observed"
    );
    assert_eq!(
        first["payload"]["resource_observations"]["items"][1]["effective_directive"],
        "script-src"
    );
    // Non-allowlisted tag_name is redacted under broad `redact_fields: ["tag"]`.
    assert_eq!(
        events[1]["payload"]["browser_event"]["target"]["tag_name"],
        "[REDACTED]"
    );
    // Diagnostic context resource_observation is preserved while custom context is redacted.
    assert_eq!(events[2]["event_type"], "diagnostic_context");
    assert_eq!(events[2]["payload"]["failure_ref"], failure_ref);
    assert_eq!(events[2]["context"]["payout"]["reference"], "[REDACTED]");

    // Exact-key redaction masks `tag` while preserving `tag_name` including non-allowlisted tags.
    let exact_dir = tempfile::tempdir().unwrap();
    let mut exact_config = Config::local("checkout", exact_dir.path().join("events"))
        .with_redact_exact_fields(["tag"]);
    exact_config.evidence_capabilities = Some(json!({
        "version": 1,
        "features": debugbundle::wire::FEATURES
    }));
    let exact_relay = Relay::new(Client::new(exact_config), RelayOptions::default());
    let exact_body = serde_json::to_vec(&json!({
        "batch": [build_frontend("77777777-7777-4777-8777-777777777777", "div")]
    }))
    .unwrap();
    let exact_response = exact_relay.handle(&head, &exact_body).await;
    assert_eq!(exact_response.status, 202);
    assert_eq!(exact_response.body.as_ref().unwrap()["accepted"], 1);
    let exact_file = std::fs::read_dir(exact_dir.path().join("events"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let exact_events: Value =
        serde_json::from_slice(&std::fs::read(exact_file.path()).unwrap()).unwrap();
    let exact_event = &exact_events[0];
    assert_eq!(exact_event["context"]["payout"]["tag"], "[REDACTED]");
    assert_eq!(exact_event["context"]["payout"]["tag_name"], "script");
    assert_eq!(
        exact_event["payload"]["message"],
        "chunk failed tag=[REDACTED] tag_name=script"
    );
    assert_eq!(
        exact_event["payload"]["browser_event"]["target"]["tag_name"],
        "div"
    );
    assert_eq!(
        exact_event["payload"]["browser_event"]["target"]["source_url"],
        "https://cdn.example.test/bundle.js?tag=%5BREDACTED%5D&tag_name=script&safe=1"
    );
    assert_eq!(
        exact_event["payload"]["rejection_reason"]["preview"],
        "{\"tag\":\"[REDACTED]\",\"tag_name\":\"script\"}"
    );
}
