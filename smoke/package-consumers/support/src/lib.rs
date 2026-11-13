use axum::{
    extract::State,
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use debugbundle::{Client, Config};
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const TRACE: &str = "11111111-1111-4111-8111-111111111111";
const TOKEN: &str = "synthetic-artifact-project-token";
pub struct Mock {
    pub endpoint: String,
    events: Arc<Mutex<Vec<Value>>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Mock {
    pub fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let state = events.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let thread = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let app = Router::new()
                        .route("/v1/events", post(ingest))
                        .route("/v1/sdk/config", get(config))
                        .with_state(state);
                    axum::serve(tokio::net::TcpListener::from_std(listener).unwrap(), app)
                        .with_graceful_shutdown(async {
                            let _ = rx.await;
                        })
                        .await
                        .unwrap();
                });
        });
        Self {
            endpoint: format!("http://{address}/v1/events"),
            events,
            stop: Some(tx),
            thread: Some(thread),
        }
    }
    pub fn client(&self, service: &str, spool: impl Into<std::path::PathBuf>) -> Client {
        Client::new(Config {
            endpoint: self.endpoint.clone(),
            allow_insecure_http: true,
            environment: "artifact-test".into(),
            spool_dir: spool.into(),
            ..Config::new(TOKEN, service)
        })
    }
    pub fn events(&self) -> Vec<Value> {
        self.events.lock().unwrap().clone()
    }
    pub fn wait_config(&self, client: &Client) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !client.remote_config_ready() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    pub fn assert_framework(&self, framework: &str) {
        let events = self.events();
        assert!(events
            .iter()
            .any(|event| event["event_type"] == "backend_exception"
                && event["service"]["framework"] == framework
                && event["correlation"]["trace_id"] == TRACE));
        assert!(events
            .iter()
            .any(|event| event["event_type"] == "frontend_exception"
                && event["sdk_name"] == "@debugbundle/sdk-browser"
                && event["service"]["name"] == "checkout-web"
                && event["correlation"]["trace_id"] == TRACE));
        assert!(!serde_json::to_string(&events)
            .unwrap()
            .contains("BROWSER_SECRET"));
    }
}
impl Drop for Mock {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
fn authenticated(headers: &HeaderMap) {
    assert_eq!(
        headers["authorization"].to_str().unwrap(),
        format!("Bearer {TOKEN}")
    );
}
async fn config(headers: HeaderMap) -> Json<Value> {
    authenticated(&headers);
    Json(
        json!({"tier":"free","probes":{"enabled":false},"evidence_capabilities":{"version":1,"features":debugbundle::wire::FEATURES}}),
    )
}
async fn ingest(
    State(events): State<Arc<Mutex<Vec<Value>>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (axum::http::StatusCode, Json<Value>) {
    authenticated(&headers);
    assert!(
        headers.get("cookie").is_none(),
        "browser cookies must never reach ingestion"
    );
    assert!(serde_json::to_vec(&body).unwrap().len() <= debugbundle::wire::MAX_BODY_BYTES);
    let batch = body["events"].as_array().unwrap();
    for event in batch {
        assert!(
            debugbundle::wire::valid_event(event)
                || debugbundle::wire::valid_analytics_event(event)
        );
        assert!(event.get("project_token").is_none());
    }
    events.lock().unwrap().extend(batch.iter().cloned());
    (
        axum::http::StatusCode::ACCEPTED,
        Json(
            json!({"accepted":batch.len(),"rejected":0,"errors":[],"evidence_capabilities":{"version":1,"features":debugbundle::wire::FEATURES}}),
        ),
    )
}
pub fn browser_batch() -> Value {
    let fixtures: Value = serde_json::from_str(include_str!("../relay-compliance.json")).unwrap();
    let mut event = fixtures["cases"][0]["request"]["bodyJson"]["batch"][0].clone();
    event["project_token"] = json!("BROWSER_SECRET");
    event["organization_id"] = json!("BROWSER_SECRET");
    json!({"batch":[event]})
}
