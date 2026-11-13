use super::*;
#[test]
fn first_three_then_checkpoints_and_silence_reset() {
    let mut suppression = Suppression::default();
    let start = Instant::now();
    for n in 0..15 {
        assert_eq!(
            suppression.observe("same", start, "2026-11-12T00:00:00.000Z"),
            n < 3
        );
    }
    let summary = suppression.collect(start + Duration::from_secs(30), 1);
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].count, 12);
    assert!(!suppression.observe(
        "same",
        start + Duration::from_secs(31),
        "2026-11-12T00:00:31.000Z"
    ));
    assert!(suppression.observe(
        "same",
        start + Duration::from_secs(92),
        "2026-11-12T00:01:32.000Z"
    ));
}
#[test]
fn ordinary_window_resets_without_cross_key_suppression() {
    let mut suppression = Suppression::default();
    let start = Instant::now();
    for _ in 0..3 {
        assert!(suppression.observe("first", start, "2026-11-12T00:00:00.000Z"));
    }
    assert!(!suppression.observe("first", start, "2026-11-12T00:00:00.000Z"));
    assert!(suppression.observe("second", start, "2026-11-12T00:00:00.000Z"));
    assert!(suppression.observe(
        "first",
        start + Duration::from_secs(31),
        "2026-11-12T00:00:31.000Z"
    ));
}
