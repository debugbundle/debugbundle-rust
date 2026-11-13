mod helpers;
use debugbundle::{
    delivery::{Transport, TransportFailure, TransportResponse},
    Client, Config, Context, ExceptionSnapshot, Level, ProbeOptions,
};
use helpers::{configured, Recorder};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
fn deep() -> Value {
    let mut value = Value::Null;
    for _ in 0..50_000 {
        value = Value::Array(vec![value]);
    }
    value
}
#[test]
fn untrusted_owned_json_is_released_without_recursive_stack_overflow() {
    for case in ["hook", "probe", "transport", "configuration", "panic"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "owned_json_child", "--nocapture"])
            .env("DEBUGBUNDLE_JSON_TEST_CASE", case)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{case}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
struct UntrustedResponse(Arc<AtomicUsize>);
impl Transport for UntrustedResponse {
    fn send(&self, _: &[u8], _: Duration) -> Result<TransportResponse, TransportFailure> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(TransportResponse {
            status: 202,
            body: Some(deep()),
            retry_after: None,
        })
    }
}
#[test]
fn owned_json_child() {
    let Ok(case) = std::env::var("DEBUGBUNDLE_JSON_TEST_CASE") else {
        return;
    };
    let recorder = Arc::new(Recorder::default());
    let mut config = configured(recorder.clone());
    match case.as_str() {
        "configuration" => {
            let mut proof = json!({"version":1,"features":[]});
            proof["features"] = deep();
            config.evidence_capabilities = Some(proof);
            let client = Client::new(config);
            assert_eq!(client.status().reason, Some("invalid_configuration"));
        }
        "hook" => {
            config.before_send = Some(Arc::new(|mut event| {
                let mut context = serde_json::Map::new();
                context.insert("deep".into(), deep());
                event["context"] = Value::Object(context);
                Some(event)
            }));
            let client = Client::new(config);
            client.capture_log(Level::Error, "bounded", &Context::default());
            assert!(client.flush(Duration::from_secs(3)).drained);
            assert!(recorder.events.lock().unwrap().is_empty());
        }
        "panic" => {
            config.before_send = Some(Arc::new(|_| std::panic::panic_any(deep())));
            let client = Client::new(config);
            client.capture_log(Level::Error, "bounded", &Context::default());
            assert!(client.flush(Duration::from_secs(3)).drained);
            assert!(recorder.events.lock().unwrap().is_empty());
        }
        "probe" => {
            let client = Client::new(config);
            let producer = Arc::new(deep);
            client.probe_lazy(
                "bounded",
                &producer,
                ProbeOptions::default(),
                &Context::default(),
            );
            assert!(client.flush(Duration::from_secs(3)).drained);
            client.capture_exception(
                &ExceptionSnapshot::new("Failure", "failed"),
                &Context::default(),
            );
            assert!(client.flush(Duration::from_secs(3)).drained);
            assert_eq!(recorder.events.lock().unwrap().len(), 1);
        }
        "transport" => {
            let calls = Arc::new(AtomicUsize::new(0));
            let client = Client::new(Config {
                transport: Some(Arc::new(UntrustedResponse(calls.clone()))),
                max_event_age: Duration::from_millis(500),
                ..Config::new("synthetic", "service")
            });
            client.capture_log(Level::Error, "bounded", &Context::default());
            assert!(client.flush(Duration::from_secs(3)).drained);
            assert!(calls.load(Ordering::SeqCst) > 0);
            assert!(client.last_event_at().is_none());
        }
        _ => panic!("unknown isolated test case"),
    }
}
