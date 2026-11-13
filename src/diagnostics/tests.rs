use super::*;
fn event(id: usize, failure: usize, attempt: usize, at: DateTime<Utc>) -> Value {
    json!({"event_id":format!("{id:08x}-1111-4111-8111-111111111111"),"event_type":"diagnostic_context","occurred_at":at.to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"payload":{"version":1,"kind":"recovery","failure_ref":format!("{failure:08x}-2222-4222-8222-222222222222"),"scope":{"kind":"operation","operation_id":"33333333-3333-4333-8333-333333333333"},"attempt_id":format!("{attempt:08x}-4444-4444-8444-444444444444"),"state":"attempt_started"}})
}
#[test]
fn cardinality_and_joint_bytes_remain_charged_after_release() {
    let now = Instant::now();
    let wall = Utc::now();
    let mut ledger = Ledger::default();
    for id in 0..20 {
        let value = event(id, 0, 0, wall);
        assert!(ledger.reserve(&value, 512, now, wall));
        ledger.release(value["event_id"].as_str().unwrap());
    }
    assert!(!ledger.reserve(&event(21, 0, 0, wall), 512, now, wall));
    assert!(ledger.held == 0 && ledger.bytes() > 0);
    let mut ledger = Ledger::default();
    for attempt in 0..5 {
        let value = event(attempt, 0, attempt, wall);
        assert!(ledger.reserve(&value, 512, now, wall));
        ledger.release(value["event_id"].as_str().unwrap());
    }
    assert!(!ledger.reserve(&event(6, 0, 6, wall), 512, now, wall));
    let mut ledger = Ledger::default();
    for failure in 0..50 {
        let value = event(failure, failure, 0, wall);
        assert!(ledger.reserve(&value, 1, now, wall));
        ledger.release(value["event_id"].as_str().unwrap());
    }
    assert!(!ledger.reserve(&event(51, 51, 0, wall), 1, now, wall));
    let mut ledger = Ledger::default();
    assert!(ledger.reserve(&event(1, 0, 0, wall), 32600, now, wall));
    assert!(!ledger.reserve(&event(2, 0, 0, wall), 32600, now, wall));
    assert!(ledger.bytes() <= MAX_BYTES);
}
#[test]
fn expiry_future_time_and_colliding_replacements_fail_closed() {
    let now = Instant::now();
    let wall = Utc::now();
    let mut ledger = Ledger::default();
    assert!(!ledger.reserve(
        &event(1, 0, 0, wall + chrono::Duration::seconds(61)),
        1,
        now,
        wall
    ));
    assert!(ledger.reserve(
        &event(1, 0, 0, wall + chrono::Duration::seconds(60)),
        1,
        now,
        wall
    ));
    assert!(!ledger.eligible(
        event(1, 0, 0, wall)["event_id"].as_str().unwrap(),
        &event(1, 0, 0, wall),
        now + WINDOW
    ));
    ledger.release(event(1, 0, 0, wall)["event_id"].as_str().unwrap());
    assert!(ledger.reserve(
        &event(2, 0, 0, wall + chrono::Duration::minutes(5)),
        1,
        now + WINDOW,
        wall + chrono::Duration::minutes(5)
    ));
    assert!(ledger.reserve(
        &event(3, 0, 0, wall + chrono::Duration::minutes(5)),
        1,
        now + WINDOW,
        wall + chrono::Duration::minutes(5)
    ));
    let previous = event(2, 0, 0, wall)["event_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!ledger.replace(
        Some(&previous),
        &event(3, 0, 0, wall + chrono::Duration::minutes(5)),
        1,
        now + WINDOW,
        wall + chrono::Duration::minutes(5)
    ));
    assert_eq!(ledger.pending.len(), 1);
}
#[test]
fn hook_timestamp_changes_do_not_escape_first_admission_limits() {
    let now = Instant::now();
    let wall = Utc::now();
    let value = event(1, 0, 0, wall);
    let id = value["event_id"].as_str().unwrap();
    let mut ledger = Ledger::default();
    assert!(ledger.reserve(&value, 512, now, wall));
    assert!(!ledger.replace(
        Some(id),
        &event(1, 0, 0, wall + chrono::Duration::hours(1)),
        512,
        now,
        wall
    ));
}
