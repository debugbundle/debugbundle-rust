#![cfg(all(feature = "local", unix))]
use debugbundle::{storage::SecureDirectory, wire::valid_event};
use serde_json::{json, Value};
use std::os::unix::fs::{symlink, PermissionsExt};
fn event() -> Value {
    json!({"schema_version":"2026-03-01","event_id":uuid::Uuid::new_v4().to_string(),"event_type":"log_event","sdk_name":"@debugbundle/sdk-rust","sdk_version":"1.0.0-alpha.1","service":{"name":"checkout","environment":"test","runtime":"rust"},"occurred_at":"2026-11-12T15:00:00.000Z","payload":{"level":"warning","message":"password=SYNTHETIC_SECRET","attributes":{}}})
}
#[test]
fn local_files_are_private_protected_canonical_arrays() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owned/events");
    let directory = SecureDirectory::open(&root).unwrap();
    let path = directory.write_events(&[event()], "checkout").unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("SYNTHETIC_SECRET"));
    let events: Vec<Value> = serde_json::from_slice(&bytes).unwrap();
    assert!(valid_event(&events[0]));
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(root).unwrap().permissions().mode() & 0o777,
        0o700
    );
}
#[test]
fn symlink_and_unsafe_service_paths_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let link = temp.path().join("link");
    symlink(outside.path(), &link).unwrap();
    assert!(SecureDirectory::open(&link.join("events")).is_err());
    assert!(SecureDirectory::open(&temp.path().join("../escape")).is_err());
    let directory = SecureDirectory::open(&temp.path().join("events")).unwrap();
    assert!(directory.write_events(&[event()], "../escape").is_err());
    assert!(outside.path().read_dir().unwrap().next().is_none());
}

#[test]
fn owned_reads_reject_hard_link_aliases_and_replaced_directories() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("events");
    let directory = SecureDirectory::open(&root).unwrap();
    let path = directory.write_events(&[event()], "checkout").unwrap();
    let alias = temp.path().join("alias.json");
    std::fs::hard_link(&path, &alias).unwrap();
    assert!(directory
        .read(path.file_name().unwrap().to_str().unwrap())
        .is_err());
    std::fs::remove_file(alias).unwrap();
    std::fs::rename(&root, temp.path().join("moved-events")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), &root).unwrap();
    assert!(directory.write_events(&[event()], "checkout").is_err());
    assert!(outside.path().read_dir().unwrap().next().is_none());
}

#[tokio::test]
async fn local_sender_uses_a_safe_filename_without_changing_final_service_identity() {
    use debugbundle::{Client, Config, Context, Level};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("events");
    let client = Client::new(Config {
        before_send: Some(std::sync::Arc::new(|mut event| {
            event["service"]["name"] = json!("worker.eu-west.v2");
            Some(event)
        })),
        ..Config::local("checkout.api", &root)
    });
    client.capture_log(Level::Error, "synthetic", &Context::default());
    let outcome = client.flush_async(std::time::Duration::from_secs(2)).await;
    assert_eq!(outcome.accepted_events, 1);
    let file = std::fs::read_dir(&root).unwrap().next().unwrap().unwrap();
    assert!(file
        .file_name()
        .to_str()
        .unwrap()
        .ends_with("-worker-eu-west-v2.events.json"));
    let events: Value = serde_json::from_slice(&std::fs::read(file.path()).unwrap()).unwrap();
    assert_eq!(events[0]["service"]["name"], "worker.eu-west.v2");
}
