use super::*;

fn head() -> RelayRequest {
    RelayRequest {
        method: "POST".into(),
        scheme: "http".into(),
        host: "internal:3000".into(),
        origin: Some("https://app.example.test".into()),
        peer: Some("192.0.2.4".parse().unwrap()),
        forwarded_host: Some("app.example.test".into()),
        forwarded_proto: Some("https".into()),
        forwarded_for: Some("203.0.113.8".into()),
        ..Default::default()
    }
}

#[test]
fn forwarded_identity_is_accepted_only_from_the_exact_configured_proxy_peer() {
    let mut request = head();
    let mut options = RelayOptions::default();
    assert_eq!(origin(&request, &options), None);
    assert_eq!(peer(&request, &options), request.peer);
    options.trusted_proxies.push(request.peer.unwrap());
    assert_eq!(
        origin(&request, &options).as_deref(),
        Some("https://app.example.test")
    );
    assert_eq!(
        peer(&request, &options),
        Some("203.0.113.8".parse().unwrap())
    );
    for invalid in [
        "203.0.113.8, 192.0.2.1",
        "not-an-ip",
        "",
        "a-very-long-untrusted-forwarded-peer-that-cannot-be-an-ip",
    ] {
        request.forwarded_for = Some(invalid.into());
        assert_eq!(peer(&request, &options), request.peer);
    }
    for host in [
        "app.example.test/",
        "app.example.test@evil.test",
        "app.example.test,evil.test",
        "app.example.test\\evil",
    ] {
        request.forwarded_host = Some(host.into());
        assert_eq!(origin(&request, &options), None);
    }
}

#[test]
fn referer_fallback_never_overrides_an_explicit_invalid_origin() {
    let mut request = head();
    let options = RelayOptions {
        allowed_origins: vec!["https://app.example.test".into()],
        ..Default::default()
    };
    request.origin = None;
    request.referer = Some("https://app.example.test/checkout?technical=1".into());
    assert_eq!(
        origin(&request, &options).as_deref(),
        Some("https://app.example.test")
    );
    for invalid in [
        "null",
        "https://app.example.test/path",
        "https://user:password@app.example.test",
        "https://app.example.test?x=1",
        "https://app.example.test#fragment",
        "ftp://app.example.test",
    ] {
        request.origin = Some(invalid.into());
        assert_eq!(origin(&request, &options), None);
    }
    request.origin = Some("x".repeat(8193));
    assert_eq!(origin(&request, &options), None);
}

#[test]
fn a_full_limiter_keeps_existing_counts_and_reclaims_only_expired_windows() {
    let now = Instant::now();
    let mut limiter = Limiter::default();
    for index in 0..4096u32 {
        limiter
            .entries
            .insert(Some(IpAddr::V4((0xc0000000 + index).into())), (now, 1));
    }
    assert!(!limiter.allow(None, 60, now));
    let known = Some(IpAddr::V4(0xc0000000u32.into()));
    assert!(!limiter.allow(known, 1, now + Duration::from_secs(59)));
    assert!(limiter.allow(None, 1, now + Duration::from_secs(60)));
    assert_eq!(limiter.entries.len(), 1);
    assert!(!limiter.allow(None, 1, now + Duration::from_secs(60)));
}
