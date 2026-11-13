#![no_main]
use debugbundle::{
    relay::{Relay, RelayOptions, RelayRequest},
    testkit::RecordingTransport,
    Client, Config,
};
use libfuzzer_sys::fuzz_target;
use serde_json::json;
use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};
struct Harness {
    relay: Relay,
    record: Arc<RecordingTransport>,
    runtime: tokio::runtime::Runtime,
    _client: Client,
}
static HARNESS: LazyLock<Harness> = LazyLock::new(|| {
    let record = Arc::new(RecordingTransport::new(100).unwrap());
    let client = Client::new(Config {
        transport: Some(record.clone()),
        request_timeout: Duration::from_secs(2),
        evidence_capabilities: Some(json!({"version":1,"features":debugbundle::wire::FEATURES})),
        ..Config::new("synthetic-fuzz", "fuzz-relay")
    });
    let relay = Relay::new(
        client.clone(),
        RelayOptions {
            durable_write: false,
            rate_limit_per_minute: 10000,
            ..Default::default()
        },
    );
    Harness {
        relay,
        record,
        runtime: tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
        _client: client,
    }
});
fuzz_target!(|data: &[u8]| {
    if data.is_empty() || data.len() > 65536 {
        return;
    }
    let mut request = RelayRequest {
        method: "POST".into(),
        scheme: "https".into(),
        host: "fuzz.invalid".into(),
        origin: Some("https://fuzz.invalid".into()),
        content_type: Some("application/json".into()),
        evidence: true,
        ..Default::default()
    };
    let text = || String::from_utf8_lossy(&data[1..]).into_owned();
    match data[0] % 8 {
        1 => request.origin = Some(text()),
        2 => {
            request.origin = None;
            request.referer = Some(text());
        }
        3 => {
            request.forwarded_host = Some(text());
            request.forwarded_proto = Some(text());
        }
        4 => request.method = text(),
        5 => request.content_type = Some(text()),
        6 => request.origin = Some("null".into()),
        7 => request.host = text(),
        _ => {}
    }
    let _ = HARNESS
        .runtime
        .block_on(HARNESS.relay.handle(&request, &data[1..]));
    for event in HARNESS.record.drain() {
        assert!(
            debugbundle::wire::valid_event(&event)
                || debugbundle::wire::valid_analytics_event(&event)
        );
        assert!(event.get("project_token").is_none() && event.get("organization_id").is_none());
    }
});
