#![cfg(feature = "local")]
use debugbundle::{
    relay::{RateLimitError, RateLimitKey, RateLimitStore, Relay, RelayOptions, RelayRequest},
    Client, Config,
};
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct Shared {
    keys: Mutex<Vec<RateLimitKey>>,
}
impl RateLimitStore for Shared {
    fn allow(&self, key: RateLimitKey) -> Result<bool, RateLimitError> {
        let mut keys = self.keys.lock().unwrap();
        let allowed = !keys
            .iter()
            .any(|old| old.namespace == key.namespace && old.peer == key.peer);
        keys.push(key);
        Ok(allowed)
    }
}
fn request() -> RelayRequest {
    RelayRequest {
        method: "POST".into(),
        scheme: "http".into(),
        host: "app.example".into(),
        origin: Some("http://app.example".into()),
        content_type: Some("application/json".into()),
        peer: Some("203.0.113.9".parse().unwrap()),
        ..RelayRequest::default()
    }
}
#[tokio::test]
async fn replicas_share_only_scoped_verified_peer_keys() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(Shared::default());
    let client = Client::new(Config {
        relay_rate_limit_store: Some(store.clone()),
        ..Config::local("checkout", directory.path().join("events"))
    });
    let options = RelayOptions::default();
    let first = Relay::new(client.clone(), options.clone());
    let second = Relay::new(client, options);
    let mut request = request();
    request.forwarded_for = Some("198.51.100.99".into());
    assert_eq!(first.handle(&request, br#"{"batch":[]}"#).await.status, 202);
    assert_eq!(
        second.handle(&request, br#"{"batch":[]}"#).await.status,
        429
    );
    let keys = store.keys.lock().unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0].peer, request.peer);
    assert_eq!(keys[0].namespace.len(), 64);
    assert_eq!(keys[0].limit_per_minute, 60);
}
#[tokio::test]
async fn shared_limiter_failure_is_not_permission_to_accept() {
    struct Unavailable;
    impl RateLimitStore for Unavailable {
        fn allow(&self, _: RateLimitKey) -> Result<bool, RateLimitError> {
            Err(RateLimitError::Unavailable)
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let relay = Relay::new(
        Client::new(Config {
            relay_rate_limit_store: Some(Arc::new(Unavailable)),
            ..Config::local("checkout", directory.path().join("events"))
        }),
        RelayOptions::default(),
    );
    assert_eq!(
        relay.handle(&request(), br#"{"batch":[]}"#).await.status,
        503
    );
}
