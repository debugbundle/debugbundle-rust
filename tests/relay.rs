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
