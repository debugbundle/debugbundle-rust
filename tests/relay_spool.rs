#![cfg(feature = "local")]
use debugbundle::{
    delivery::{Transport, TransportFailure, TransportResponse},
    relay::{Relay, RelayOptions, RelayRequest},
    Client, Config,
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
#[derive(Default)]
struct Destination {
    online: AtomicBool,
    events: Mutex<Vec<Value>>,
}
impl Transport for Destination {
    fn send(&self, body: &[u8], _: Duration) -> Result<TransportResponse, TransportFailure> {
        if !self.online.load(Ordering::Acquire) {
            return Err(TransportFailure);
        }
        let body: Value = serde_json::from_slice(body).unwrap();
        let events = body["events"].as_array().unwrap();
        self.events.lock().unwrap().extend(events.iter().cloned());
        Ok(TransportResponse {
            status: 202,
            body: Some(json!({"accepted":events.len(),"rejected":0,"errors":[]})),
            retry_after: None,
        })
    }
}
fn event() -> Value {
    let source: Value =
        serde_json::from_str(include_str!("fixtures/relay-compliance.json")).unwrap();
    source["cases"][0]["request"]["bodyJson"]["batch"][0].clone()
}
fn request() -> RelayRequest {
    RelayRequest {
        method: "POST".into(),
        scheme: "https".into(),
        host: "app.example.com".into(),
        origin: Some("https://app.example.com".into()),
        content_type: Some("application/json".into()),
        peer: Some("203.0.113.10".parse().unwrap()),
        evidence: true,
        ..RelayRequest::default()
    }
}

#[tokio::test]
async fn partial_acknowledgements_never_replay_already_accepted_ids() {
    #[derive(Default)]
    struct Partial(Mutex<Vec<Vec<String>>>);
    impl Transport for Partial {
        fn send(&self, body: &[u8], _: Duration) -> Result<TransportResponse, TransportFailure> {
            let body: Value = serde_json::from_slice(body).unwrap();
            let events = body["events"].as_array().unwrap();
            let mut calls = self.0.lock().unwrap();
            calls.push(
                events
                    .iter()
                    .map(|event| event["event_id"].as_str().unwrap().to_owned())
                    .collect(),
            );
            let first = calls.len() == 1;
            Ok(TransportResponse {
                status: 202,
                body: Some(if first {
                    json!({"accepted":events.len()-1,"rejected":1,"errors":[{"index":1,"reason":"rate_limited"}]})
                } else {
                    json!({"accepted":events.len(),"rejected":0,"errors":[]})
                }),
                retry_after: Some(Duration::from_millis(1)),
            })
        }
    }
    let root = tempfile::tempdir().unwrap();
    let destination = Arc::new(Partial::default());
    let client = Client::new(Config {
        spool_dir: root.path().join("spool"),
        transport: Some(destination.clone()),
        ..Config::new("synthetic", "checkout")
    });
    let relay = Relay::new(client, RelayOptions::default());
    let first = event();
    let mut second = event();
    second["event_id"] = json!(uuid::Uuid::new_v4().to_string());
    let body = serde_json::to_vec(&json!({"batch":[first,second]})).unwrap();
    assert_eq!(relay.handle(&request(), &body).await.status, 202);
    assert!(relay.flush_async(Duration::from_secs(3)).await);
    let calls = destination.0.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].len(), 2);
    assert_eq!(
        calls[1],
        vec![second["event_id"].as_str().unwrap().to_owned()]
    );
    assert_eq!(relay.status().delivered_records, 1);
}

#[tokio::test]
async fn bodyless_legacy_success_keeps_durable_records_pending() {
    struct Legacy;
    impl Transport for Legacy {
        fn requires_acknowledgement(&self) -> bool {
            false
        }
        fn send(&self, _: &[u8], _: Duration) -> Result<TransportResponse, TransportFailure> {
            Ok(TransportResponse {
                status: 202,
                body: None,
                retry_after: None,
            })
        }
    }
    let root = tempfile::tempdir().unwrap();
    let config = Config {
        spool_dir: root.path().join("spool"),
        transport: Some(Arc::new(Legacy)),
        ..Config::new("synthetic", "checkout")
    };
    let client = Client::new(config.clone());
    let relay = Relay::new(client, RelayOptions::default());
    let body = serde_json::to_vec(&json!({"batch":[event()]})).unwrap();
    assert_eq!(relay.handle(&request(), &body).await.status, 202);
    assert!(!relay.flush_async(Duration::from_millis(150)).await);
    assert_eq!(relay.status().delivered_records, 0);
    assert!(!std::fs::read_dir(&config.spool_dir)
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().ends_with(".delivered")));
}

#[tokio::test]
async fn credential_rotation_never_replays_a_different_project_spool() {
    let root = tempfile::tempdir().unwrap();
    let destination = Arc::new(Destination::default());
    let mut config = Config {
        spool_dir: root.path().join("spool"),
        transport: Some(destination.clone()),
        ..Config::new("synthetic-project-A", "checkout")
    };
    let client = Client::new(config.clone());
    let relay = Relay::new(client.clone(), RelayOptions::default());
    let body = serde_json::to_vec(&json!({"batch":[event()]})).unwrap();
    assert_eq!(relay.handle(&request(), &body).await.status, 202);
    drop(relay);
    let _ = client.shutdown_async(Duration::from_millis(100)).await;
    drop(client);
    config.project_token = "synthetic-project-B".into();
    destination.online.store(true, Ordering::Release);
    let relay = Relay::new(Client::new(config.clone()), RelayOptions::default());
    assert!(relay.flush_async(Duration::from_secs(3)).await);
    assert!(destination.events.lock().unwrap().is_empty());
    assert_eq!(relay.status().delivered_records, 0);
    assert!(std::fs::read_dir(config.spool_dir)
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry
            .file_name()
            .to_string_lossy()
            .ends_with(".events.json")));
}

#[cfg(unix)]
#[tokio::test]
async fn forged_delivery_markers_never_create_a_canonical_receipt() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let destination = Arc::new(Destination::default());
    let config = Config {
        spool_dir: root.path().join("spool"),
        transport: Some(destination.clone()),
        ..Config::new("synthetic", "checkout")
    };
    let relay = Relay::new(Client::new(config.clone()), RelayOptions::default());
    let original = event();
    let body = serde_json::to_vec(&json!({"batch":[original]})).unwrap();
    assert_eq!(relay.handle(&request(), &body).await.status, 202);
    let array = std::fs::read_dir(&config.spool_dir)
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .ends_with(".events.json")
        })
        .unwrap();
    let marker = config
        .spool_dir
        .join(format!("{}.delivered", array.file_name().to_string_lossy()));
    std::fs::write(&marker, b"{\"delivered\":true}").unwrap();
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!relay.flush_async(Duration::from_millis(150)).await);
    assert_eq!(relay.status().delivered_records, 0);
    assert!(destination.events.lock().unwrap().is_empty());
    std::fs::remove_file(marker).unwrap();
    destination.online.store(true, Ordering::Release);
    assert!(relay.flush_async(Duration::from_secs(3)).await);
    let events = destination.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event_id"], original["event_id"]);
}
#[tokio::test]
async fn durable_admission_survives_offline_restart_and_preserves_event_identity() {
    let root = tempfile::tempdir().unwrap();
    let destination = Arc::new(Destination::default());
    let config = Config {
        spool_dir: root.path().join("spool"),
        transport: Some(destination.clone()),
        request_timeout: Duration::from_secs(1),
        ..Config::new("synthetic", "checkout")
    };
    let client = Client::new(config.clone());
    let relay = Relay::new(client.clone(), RelayOptions::default());
    let original = event();
    let body = serde_json::to_vec(&json!({"batch":[original]})).unwrap();
    let response = relay.handle(&request(), &body).await;
    assert_eq!(response.status, 202);
    assert_eq!(response.body.unwrap()["accepted"], 1);
    let arrays = std::fs::read_dir(&config.spool_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .ends_with(".events.json")
        })
        .collect::<Vec<_>>();
    assert_eq!(arrays.len(), 1);
    let saved: Value = serde_json::from_slice(&std::fs::read(arrays[0].path()).unwrap()).unwrap();
    assert_eq!(saved[0]["event_id"], original["event_id"]);
    assert!(saved[0].get("project_token").is_none());
    drop(relay);
    let _ = client.shutdown_async(Duration::from_millis(100)).await;
    drop(client);
    destination.online.store(true, Ordering::Release);
    let client = Client::new(config);
    let relay = Relay::new(client.clone(), RelayOptions::default());
    assert!(relay.flush_async(Duration::from_secs(3)).await);
    let events = destination.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event_id"], original["event_id"]);
    drop(events);
    assert!(relay.status().delivered_records >= 1);
}
