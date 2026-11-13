#![no_main]
use debugbundle::{privacy::Privacy, protocol, wire};
use libfuzzer_sys::fuzz_target;
use serde_json::Value;
use std::sync::LazyLock;
static PRIVACY: LazyLock<Privacy> = LazyLock::new(|| Privacy::new(&[]).unwrap());
fuzz_target!(|data: &[u8]| {
    if data.len() > 65536 {
        return;
    }
    let Ok(input) = serde_json::from_slice::<Value>(data) else {
        return;
    };
    let _ = wire::valid_event(&input);
    let _ = wire::valid_analytics_event(&input);
    let _ = wire::valid_reference(&input);
    let _ = wire::capabilities(&input);
    let _ = wire::canonical_size(&input);
    let count = input["count"]
        .as_u64()
        .map(|value| value.min(101) as usize)
        .unwrap_or(1);
    if let Some(ack) = protocol::acknowledgement(input.get("ack").unwrap_or(&input), count) {
        let mut indices = ack
            .accepted
            .into_iter()
            .chain(ack.retry)
            .chain(ack.terminal)
            .collect::<Vec<_>>();
        indices.sort_unstable();
        assert_eq!(indices, (0..count).collect::<Vec<_>>());
    }
    if let Some(protected) = PRIVACY.protect(&input) {
        assert!(serde_json::to_vec(&protected).unwrap().len() <= 256 * 1024);
        // Malformed encoded text can become more restrictive on later passes,
        // matching the shared sanitizer. Do not invent whole-domain idempotence.
        let _ = PRIVACY.protect(&protected);
    }
    let wrapped = serde_json::json!({"password":String::from_utf8_lossy(data),"value":input});
    if let Some(protected) = PRIVACY.protect(&wrapped) {
        assert_eq!(protected["password"], "[REDACTED]");
        assert!(serde_json::to_vec(&protected).unwrap().len() <= 256 * 1024);
    }
});
