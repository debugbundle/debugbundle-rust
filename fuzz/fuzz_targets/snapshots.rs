#![no_main]
use debugbundle::{ExceptionSnapshot, FrameSnapshot, TraceOrigin};
use libfuzzer_sys::fuzz_target;
use serde_json::Value;
use std::sync::LazyLock;
static BASE: LazyLock<Value> = LazyLock::new(|| {
    let corpus: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/evidence-wire-cases.json"
    ))
    .unwrap();
    corpus["events"][0]["event"].clone()
});
struct Opaque;
impl std::fmt::Debug for Opaque {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        panic!("must not debug-format an opaque error")
    }
}
impl std::fmt::Display for Opaque {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        panic!("must not format an opaque error")
    }
}
impl std::error::Error for Opaque {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        panic!("must not traverse an opaque error")
    }
}
fuzz_target!(|data: &[u8]| {
    if data.len() > 16384 {
        return;
    }
    let text = String::from_utf8_lossy(data);
    let child = if data.first().is_some_and(|byte| byte % 5 == 0) {
        debugbundle::ExceptionInput::borrowed(&Opaque).snapshot()
    } else {
        ExceptionSnapshot::new("FuzzChild", &text)
    }
    .with_stack(&text, TraceOrigin::RuntimeReported);
    let mut snapshot = ExceptionSnapshot::new(&text, &text);
    for byte in data.iter().take(16) {
        snapshot = match byte % 4 {
            0 => snapshot.with_cause(&child),
            1 => snapshot.with_aggregate(std::slice::from_ref(&child)),
            2 => ExceptionSnapshot::new("Outer", &text).with_cause(&snapshot),
            _ => snapshot.with_frames(
                &vec![
                    FrameSnapshot {
                        function: Some(text.to_string()),
                        ..Default::default()
                    };
                    usize::from(*byte) % 40
                ],
                TraceOrigin::RuntimeReported,
            ),
        };
        let mut event = BASE.clone();
        event["payload"]["exception_evidence"] = snapshot.evidence().clone();
        assert!(debugbundle::wire::valid_event(&event));
    }
});
