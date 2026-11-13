use super::*;
#[test]
fn remote_flags_and_cadence_are_strict() {
    let free=Remote::parse(&json!({"probes_enabled":true,"remote_probes_enabled":false,"trigger_token_key":"must-not-arm"}),Duration::from_secs(60)).unwrap();
    assert!(!free.remote_enabled);
    assert!(free.key.is_empty());
    let disabled = Remote::parse(
        &json!({"probes_enabled":false,"remote_probes_enabled":true}),
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(!disabled.enabled);
    assert!(!disabled.remote_enabled);
    assert!(Remote::parse(&json!({"probes_enabled":"true"}), Duration::from_secs(60)).is_none());
    let expires = (Utc::now() + chrono::Duration::seconds(30)).to_rfc3339();
    let directive = json!({"id":"11111111-1111-4111-8111-111111111111","label_pattern":"checkout.*","service":"*","environment":"test","expires_at":expires});
    let mut paid=Remote::parse(&json!({"probes_enabled":true,"remote_probes_enabled":true,"active_probes":[directive.clone()]}),Duration::from_secs(60)).unwrap();
    assert_eq!(paid.interval, Duration::from_secs(15));
    assert!(paid.matching("checkout", "checkout", "test").len() == 1);
    assert!(paid
        .matching("checkout-other", "checkout", "test")
        .is_empty());
    paid.piggyback(&json!({}), &crate::privacy::Privacy::new(&[]).unwrap());
    assert!(paid.directives.is_empty());
    assert_eq!(paid.interval, Duration::from_secs(60));
    let mut too_long = directive;
    too_long["expires_at"] = json!((Utc::now() + chrono::Duration::seconds(3601)).to_rfc3339());
    assert!(Directive::parse(&too_long, "expires_at", 3600).is_none());
}
