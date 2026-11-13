use super::*;
#[test]
fn receipts_require_exact_token_operation_and_lifetime() {
    let now = Instant::now();
    let mut registry = Registry::default();
    registry.record("one", "operation", "event", now, Duration::from_secs(30));
    assert!(registry.contains("one", "operation", now));
    assert!(!registry.contains("another", "operation", now));
    assert!(!registry.contains("one", "another", now));
    assert!(!registry.contains("one", "operation", now + Duration::from_secs(30)));
    registry.revoke("event");
    assert!(!registry.contains("one", "operation", now));
    for id in 0..130 {
        registry.record(&id.to_string(), "op", "event", now, Duration::from_secs(30));
    }
    assert_eq!(registry.receipts.len(), 128);
}
