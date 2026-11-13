#![no_main]
use debugbundle::storage::SecureDirectory;
use libfuzzer_sys::fuzz_target;
use serde_json::Value;
use std::sync::LazyLock;
struct Harness {
    root: tempfile::TempDir,
    directory: SecureDirectory,
    event: Value,
}
static HARNESS: LazyLock<Harness> = LazyLock::new(|| {
    let root = tempfile::tempdir().unwrap();
    let directory = SecureDirectory::open(&root.path().join("events")).unwrap();
    let corpus: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/evidence-wire-cases.json"
    ))
    .unwrap();
    Harness {
        root,
        directory,
        event: corpus["events"][0]["event"].clone(),
    }
});
fuzz_target!(|data: &[u8]| {
    if data.len() > 256 {
        return;
    }
    let name = String::from_utf8_lossy(data);
    if let Ok(path) = HARNESS
        .directory
        .write_events(std::slice::from_ref(&HARNESS.event), &name)
    {
        assert!(path.starts_with(HARNESS.root.path().join("events")));
        let file = path.file_name().unwrap().to_str().unwrap();
        let events: Vec<Value> =
            serde_json::from_slice(&HARNESS.directory.read(file).unwrap()).unwrap();
        assert_eq!(events.len(), 1);
        assert!(debugbundle::wire::valid_event(&events[0]));
        std::fs::remove_file(path).unwrap();
    }
    // Lookup names never become paths, even when the producer uses traversal,
    // rooted syntax, device/stream punctuation, Unicode or control characters.
    let _ = HARNESS.directory.read(&name);
});
