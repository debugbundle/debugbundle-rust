#![cfg(all(feature = "local", windows))]
use debugbundle::{
    storage::SecureDirectory, wire::valid_event, Client, Config, Context, ExceptionSnapshot,
};
use serde_json::Value;
use std::{process::Command, time::Duration};

#[test]
fn private_windows_output_is_a_protected_canonical_array_and_keeps_local_only_scope() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("events");
    let client = Client::new(Config::local("windows-worker", &root));
    assert!(client.status().reason.is_none());
    client.capture_exception(
        &ExceptionSnapshot::new("WindowsFailure", "password=SYNTHETIC_SECRET"),
        &Context::default(),
    );
    assert!(client.flush(Duration::from_secs(5)).drained);
    let file = std::fs::read_dir(&root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let bytes = std::fs::read(&file).unwrap();
    let events: Vec<Value> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(events.len(), 1);
    assert!(valid_event(&events[0]));
    assert!(!String::from_utf8_lossy(&bytes).contains("SYNTHETIC_SECRET"));
    assert!(client.shutdown(Duration::from_secs(2)).drained);
}

#[test]
fn windows_owned_handles_reject_hardlinks_and_keep_ancestor_names_stable() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("events");
    let directory = SecureDirectory::open(&root).unwrap();
    assert!(std::fs::rename(&root, temp.path().join("moved")).is_err());
    // Reuse an actual native SDK envelope rather than a second schema fixture.
    let client = Client::new(Config::local("windows-worker", &root));
    client.capture_exception(
        &ExceptionSnapshot::new("WindowsFailure", "synthetic"),
        &Context::default(),
    );
    assert!(client.flush(Duration::from_secs(5)).drained);
    let file = std::fs::read_dir(&root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::hard_link(&file, temp.path().join("alias.json")).unwrap();
    assert!(directory
        .read(file.file_name().unwrap().to_str().unwrap())
        .is_err());
    assert!(client.shutdown(Duration::from_secs(2)).drained);
}

#[test]
fn windows_reparse_ancestors_and_widened_file_acls_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let junction = temp.path().join("junction");
    let created = Command::new("cmd")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&junction)
        .arg(outside.path())
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "native junction fixture must be available"
    );
    assert!(SecureDirectory::open(&junction.join("events")).is_err());
    assert!(outside.path().read_dir().unwrap().next().is_none());
    let root = temp.path().join("events");
    let client = Client::new(Config::local("windows-worker", &root));
    client.capture_exception(
        &ExceptionSnapshot::new("WindowsFailure", "synthetic"),
        &Context::default(),
    );
    assert!(client.flush(Duration::from_secs(5)).drained);
    let file = std::fs::read_dir(&root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let changed = Command::new("icacls")
        .arg(&file)
        .args(["/grant", "*S-1-1-0:(R)"])
        .output()
        .unwrap();
    assert!(
        changed.status.success(),
        "native ACL fixture must be available"
    );
    let directory = SecureDirectory::open(&root).unwrap();
    assert!(directory
        .read(file.file_name().unwrap().to_str().unwrap())
        .is_err());
    assert!(client.shutdown(Duration::from_secs(2)).drained);
}
