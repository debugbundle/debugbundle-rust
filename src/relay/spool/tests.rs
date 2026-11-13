use super::*;
use serde_json::json;
fn event() -> Value {
    let source: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/relay-compliance.json"
    ))
    .unwrap();
    source["cases"][0]["request"]["bodyJson"]["batch"][0].clone()
}
fn retained(store: &Store, now: i64) -> (Record, Vec<u8>) {
    let scope = scope("https://ingest.example/v1/events", "synthetic");
    store.admit(vec![event()], &scope, &[], now).unwrap();
    let entry = store
        .directory
        .entries(MAX_FILES)
        .unwrap()
        .into_iter()
        .find(|entry| entry.name.ends_with(STATE))
        .unwrap();
    let raw = store.directory.read(&entry.name).unwrap();
    (store.decode(&raw).unwrap(), raw)
}
fn cleanup(store: &Store, now: i64) -> io::Result<()> {
    let _guard = store.directory.lock(GUARD)?;
    store.cleanup_locked(now)
}
fn complete(store: &Store, mut record: Record, previous: &[u8], now: i64) -> Record {
    let data: Vec<Value> =
        serde_json::from_slice(&store.directory.read(&record.file).unwrap()).unwrap();
    record.states.fill(Disposition::Accepted);
    record.acknowledged_digests = data
        .iter()
        .map(|event| Some(digest(&serde_json::to_vec(event).unwrap())))
        .collect();
    record.completed_at = Some(now);
    store.update(&record, previous).unwrap();
    record
}
#[test]
fn authenticated_metadata_rejects_forged_progress_and_unknown_hops() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("spool")).unwrap();
    let (record, raw) = retained(&store, 1_000_000);
    let mut forged: Value = serde_json::from_slice(&raw).unwrap();
    forged["record"]["states"][0] = json!("accepted");
    forged["record"]["acknowledged_digests"][0] = json!("0".repeat(64));
    assert!(store.decode(&serde_json::to_vec(&forged).unwrap()).is_err());
    let mut unsupported = record;
    unsupported.proof = vec![wire::descriptor(&[]); 9];
    assert!(store.encode(&unsupported).is_err());
}
#[test]
fn retention_uses_local_creation_and_canonical_completion_times() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("spool")).unwrap();
    let (record, _) = retained(&store, 1_000_000);
    cleanup(&store, 1_000_000 + 7 * 24 * 60 * 60 * 1000 - 1).unwrap();
    assert!(store.directory.read(&record.file).is_ok());
    cleanup(&store, 1_000_000 + 7 * 24 * 60 * 60 * 1000).unwrap();
    assert!(store.directory.read(&record.file).is_err());
    let (record, raw) = retained(&store, 2_000_000);
    let record = complete(&store, record, &raw, 2_000_100);
    cleanup(&store, 2_000_100 + 24 * 60 * 60 * 1000 - 1).unwrap();
    assert!(store.directory.read(&record.file).is_ok());
    cleanup(&store, 2_000_100 + 24 * 60 * 60 * 1000).unwrap();
    assert!(store.directory.read(&record.file).is_err());
}
#[test]
fn retention_recovers_a_completion_left_beside_its_old_pending_state() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("spool")).unwrap();
    let (record, raw) = retained(&store, 1_000_000);
    let record = complete(&store, record, &raw, 1_000_100);
    store
        .directory
        .write_named(&state_name(&record.file), &raw)
        .unwrap();
    cleanup(&store, 1_000_100 + 24 * 60 * 60 * 1000).unwrap();
    assert!(store.directory.read(&record.file).is_err());
    assert!(store.directory.read(&state_name(&record.file)).is_err());
    assert!(store.directory.read(&delivered_name(&record.file)).is_err());
}
#[test]
fn retention_recovers_after_an_expired_array_was_already_removed() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("spool")).unwrap();
    let (record, _) = retained(&store, 1_000_000);
    let bytes = store.directory.read(&record.file).unwrap();
    store.directory.remove(&record.file, &bytes).unwrap();
    cleanup(&store, 1_000_000 + 7 * 24 * 60 * 60 * 1000).unwrap();
    assert!(store.directory.read(&state_name(&record.file)).is_err());
}

#[test]
fn accepted_progress_and_acknowledged_digests_are_immutable() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("spool")).unwrap();
    let (mut record, raw) = retained(&store, 1_000_000);
    record.states[0] = Disposition::Accepted;
    record.acknowledged_digests[0] = Some("a".repeat(64));
    let accepted = store.update(&record, &raw).unwrap();
    let mut replay = record.clone();
    replay.states[0] = Disposition::Pending;
    replay.acknowledged_digests[0] = None;
    assert!(store.update(&replay, &accepted).is_err());
    let mut changed = record;
    changed.acknowledged_digests[0] = Some("b".repeat(64));
    assert!(store.update(&changed, &accepted).is_err());
}

#[test]
fn metadata_updates_preserve_original_proof_and_index_count() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("spool")).unwrap();
    let (mut record, raw) = retained(&store, 1_000_000);
    record.proof = vec![wire::descriptor(&["exception_evidence_v1".into()])];
    assert!(store.update(&record, &raw).is_err());
    let (mut record, raw) = retained(&store, 1_000_001);
    record.states.push(Disposition::Pending);
    record.acknowledged_digests.push(None);
    assert!(store.update(&record, &raw).is_err());
}
