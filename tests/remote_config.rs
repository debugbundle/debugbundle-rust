#![cfg(feature = "http")]
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use debugbundle::{Client, Config, Context, OperationKind, OperationOptions, ProbeOptions};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

struct Server {
    endpoint: String,
    config: Arc<Mutex<Value>>,
    events: Arc<Mutex<Vec<Value>>>,
    requests: Arc<Mutex<Vec<String>>>,
    config_headers: Arc<Mutex<Vec<String>>>,
    config_status: Arc<AtomicUsize>,
    config_etag: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}
impl Server {
    fn new(config: Value) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1/events", listener.local_addr().unwrap());
        let config = Arc::new(Mutex::new(config));
        let events = Arc::new(Mutex::new(Vec::new()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let config_headers = Arc::new(Mutex::new(Vec::new()));
        let config_status = Arc::new(AtomicUsize::new(200));
        let config_etag = Arc::new(Mutex::new(None::<String>));
        let (headers, status, etag) = (
            config_headers.clone(),
            config_status.clone(),
            config_etag.clone(),
        );
        let (body, records, queries, done) = (
            config.clone(),
            events.clone(),
            requests.clone(),
            stop.clone(),
        );
        std::thread::spawn(move || {
            while !done.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                };
                // Accepted sockets can inherit the listener's nonblocking mode
                // on native platforms. This fixture uses finite blocking reads.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                let mut header_len = None;
                let mut length = 0;
                loop {
                    let n = stream.read(&mut buffer).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(i) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                        header_len = Some(i + 4);
                        length = String::from_utf8_lossy(&bytes[..i])
                            .lines()
                            .find_map(|s| {
                                s.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|s| s.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= i + 4 + length {
                            break;
                        }
                    }
                }
                // A pooled or cancelled client can close an unused connection.
                // That is not an HTTP request and must not kill the fixture.
                let Some(header_len) = header_len.filter(|offset| bytes.len() >= offset + length)
                else {
                    continue;
                };
                let text = String::from_utf8_lossy(&bytes);
                let path = text.lines().next().unwrap_or("").to_owned();
                queries.lock().unwrap().push(path.clone());
                let (reply, status, etag) = if path.starts_with("GET ") {
                    let reply = body.lock().unwrap().clone();
                    let status = status.load(Ordering::Acquire);
                    let etag = etag.lock().unwrap().clone();
                    // Publish the observation only after the selected response
                    // is immutable, so a test cannot race its status change.
                    headers.lock().unwrap().push(text.to_string());
                    (reply, status, etag)
                } else {
                    let request: Value =
                        serde_json::from_slice(&bytes[header_len..header_len + length]).unwrap();
                    let values = request["events"].as_array().unwrap();
                    records.lock().unwrap().extend(values.iter().cloned());
                    (
                        json!({"accepted":values.len(),"rejected":0,"errors":[],"evidence_capabilities":{"version":1,"features":[]}}),
                        200,
                        None,
                    )
                };
                let reply = if status == 304 {
                    Vec::new()
                } else {
                    serde_json::to_vec(&reply).unwrap()
                };
                let etag = etag
                    .map(|etag| format!("ETag: {etag}\r\n"))
                    .unwrap_or_default();
                let _=stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{etag}Connection: close\r\n\r\n",reply.len()).as_bytes());
                let _ = stream.write_all(&reply);
            }
        });
        Self {
            endpoint,
            config,
            events,
            requests,
            config_headers,
            config_status,
            config_etag,
            stop,
        }
    }
    fn client(&self) -> Client {
        Client::new(Config {
            endpoint: self.endpoint.clone(),
            allow_insecure_http: true,
            environment: "test".into(),
            batch_size: 100,
            ..Config::new("synthetic", "checkout")
        })
    }

    fn captured_requests(&self) -> Vec<(String, u16)> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| {
                // A contended best-effort call can produce this operational
                // summary. It is neither a policy leak nor a captured request.
                if event["event_type"] == "error_suppressed"
                    && event["payload"]["reason"] == "queue_pressure"
                {
                    return None;
                }
                assert_eq!(event["event_type"], "request_event", "{event:?}");
                Some((
                    event["payload"]["path"].as_str().unwrap().to_owned(),
                    u16::try_from(event["payload"]["response_status"].as_u64().unwrap()).unwrap(),
                ))
            })
            .collect()
    }

    async fn capture_request(&self, client: &Client, path: &str, status: u16) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            client.capture_request(
                &debugbundle::RequestSnapshot {
                    method: "GET".into(),
                    path: path.into(),
                    status,
                    ..Default::default()
                },
                &Context::default(),
            );
            assert!(client.flush_async(Duration::from_secs(2)).await.drained);
            // Flush does not reserve the state lock for the next nonblocking
            // capture. Retry only absent admission, never an incorrect event.
            if self.captured_requests().iter().any(|(p, _)| p == path) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "request was never admitted: {path}"
            );
            tokio::task::yield_now().await;
        }
    }
}
fn wait(client: &Client) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !client.remote_config_ready() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn policy() -> Value {
    json!({"preset":"investigative","capture_logs":"info","capture_request_events":"all","capture_probe_events":"standalone_when_activated","capture_diagnostic_context":true})
}
fn config() -> Value {
    json!({"probes_enabled":true,"remote_probes_enabled":true,"active_probes":[],"trigger_token_key":"synthetic-signing-key","capture_policy":policy()})
}

#[test]
fn configuration_fixture_ignores_closed_connections_and_keeps_serving() {
    let server = Server::new(json!({}));
    let address = server
        .endpoint
        .strip_prefix("http://")
        .unwrap()
        .strip_suffix("/v1/events")
        .unwrap();
    drop(std::net::TcpStream::connect(address).unwrap());
    let client = server.client();
    wait(&client);
    assert!(server
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|request| request.starts_with("GET ")));
    assert!(client.shutdown(Duration::from_secs(1)).drained);
}

#[tokio::test]
async fn paid_discovery_refreshes_automatically_at_the_instructed_cadence_without_spinning() {
    let mut body = config();
    body["poll_interval_ms"] = json!(15000);
    let server = Server::new(body);
    let started = Instant::now();
    let client = server.client();
    wait(&client);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let count = server.config_headers.lock().unwrap().len();
        if count >= 2 {
            assert_eq!(
                count, 2,
                "paid discovery must not create parallel poll loops"
            );
            assert!(started.elapsed() >= Duration::from_secs(14));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the paid automatic refresh never arrived"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(client.shutdown_async(Duration::from_secs(1)).await.drained);
}

#[tokio::test]
async fn etag_304_and_invalid_refresh_preserve_the_last_authenticated_policy() {
    let server = Server::new(config());
    *server.config_etag.lock().unwrap() = Some("\"config-one\"".into());
    let client = server.client();
    wait(&client);
    server.config_status.store(304, Ordering::Release);
    *server.config.lock().unwrap() = json!({"capture_policy":"must not parse a 304 body"});
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if client
            .refresh_remote_config_async(Duration::from_secs(1))
            .await
        {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    client.capture_request(
        &debugbundle::RequestSnapshot {
            method: "GET".into(),
            path: "/after-not-modified".into(),
            status: 204,
            ..Default::default()
        },
        &Context::default(),
    );
    server.config_status.store(200, Ordering::Release);
    *server.config_etag.lock().unwrap() = Some("\"invalid-must-not-cache\"".into());
    assert!(
        !client
            .refresh_remote_config_async(Duration::from_secs(1))
            .await
    );
    assert!(client.remote_config_ready());
    client.capture_request(
        &debugbundle::RequestSnapshot {
            method: "GET".into(),
            path: "/after-invalid".into(),
            status: 204,
            ..Default::default()
        },
        &Context::default(),
    );
    server.config_status.store(304, Ordering::Release);
    assert!(
        client
            .refresh_remote_config_async(Duration::from_secs(1))
            .await
    );
    assert!(server
        .config_headers
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .to_ascii_lowercase()
        .contains("if-none-match: \"config-one\""));
    assert!(client.flush_async(Duration::from_secs(2)).await.drained);
    assert_eq!(
        server.events.lock().unwrap().len(),
        2,
        "304 or malformed refresh must not downgrade the known all-request policy"
    );
}

#[tokio::test]
async fn failed_initial_discovery_stays_minimal_until_a_valid_legacy_response_arrives() {
    let server = Server::new(json!({}));
    server.config_status.store(500, Ordering::Release);
    let client = server.client();
    client.capture_request(
        &debugbundle::RequestSnapshot {
            method: "GET".into(),
            path: "/unknown-408".into(),
            status: 408,
            ..Default::default()
        },
        &Context::default(),
    );
    server.capture_request(&client, "/unknown-500", 500).await;
    assert!(!client.remote_config_ready());
    assert_eq!(server.captured_requests(), [("/unknown-500".into(), 500)]);
    server.config_status.store(200, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !client.remote_config_ready() {
        client
            .refresh_remote_config_async(Duration::from_secs(1))
            .await;
        assert!(Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    server.capture_request(&client, "/known-legacy", 408).await;
    assert_eq!(
        server.captured_requests(),
        [("/unknown-500".into(), 500), ("/known-legacy".into(), 408)],
        "valid legacy config restores compatible balanced policy"
    );
}

#[test]
fn native_policy_accepts_mathematical_integer_status_selectors() {
    let mut value = config();
    value["capture_policy"]["capture_request_events"] = json!("failures_only");
    value["capture_policy"]["immediate_client_error_statuses"] = json!([401.0]);
    value["capture_policy"]["immediate_client_error_path_rules"] =
        json!([{"status_code":402.0,"path_pattern":"/other","methods":["GET"]}]);
    let server = Server::new(value);
    let client = server.client();
    wait(&client);
    for (status, path) in [(401, "/synthetic"), (402, "/other")] {
        client.capture_request(
            &debugbundle::RequestSnapshot {
                method: "GET".into(),
                path: path.into(),
                status,
                ..debugbundle::RequestSnapshot::default()
            },
            &Context::default(),
        );
    }
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert_eq!(server.events.lock().unwrap().len(), 2);
}
fn token(key: &str, pattern: &str, service: &str, seconds: i64) -> String {
    let payload = json!({"activation_id":"11111111-1111-4111-8111-111111111111","label_pattern":pattern,"service":service,"environment":"test","trigger_expires_at":(chrono::Utc::now()+chrono::Duration::seconds(seconds)).to_rfc3339_opts(chrono::SecondsFormat::Millis,true)});
    let segment = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
    let mut hmac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).unwrap();
    hmac.update(segment.as_bytes());
    format!(
        "dbundle_probe_{segment}.{}",
        URL_SAFE_NO_PAD.encode(hmac.finalize().into_bytes())
    )
}

#[cfg(feature = "axum")]
#[tokio::test]
async fn axum_automatically_binds_only_the_current_request_trigger() {
    use axum::{body::Body, extract::Extension, http::Request, routing::get, Router};
    use tower::ServiceExt;
    let server = Server::new(config());
    let client = server.client();
    wait(&client);
    let handler_client = client.clone();
    let app = Router::new()
        .route(
            "/probe",
            get(move |Extension(context): Extension<Context>| {
                let client = handler_client.clone();
                async move {
                    client.probe(
                        "checkout.stage",
                        &json!({"technical":true}),
                        ProbeOptions { heavy: true },
                        &context,
                    );
                    "ok"
                }
            }),
        )
        .layer(debugbundle::axum::DebugBundleLayer::new(client.clone()));
    let signed = token("synthetic-signing-key", "checkout.*", "checkout", 60);
    for request in [
        Request::builder()
            .uri(format!("/probe?_debug_probe={signed}"))
            .body(Body::empty())
            .unwrap(),
        Request::builder()
            .uri(format!("/probe?_debug_probe={signed}"))
            .header("X-DebugBundle-Probe-Trigger", "invalid")
            .body(Body::empty())
            .unwrap(),
        Request::builder()
            .uri("/probe")
            .body(Body::empty())
            .unwrap(),
    ] {
        let response = app.clone().oneshot(request).await.unwrap();
        drop(response);
    }
    assert!(client.flush_async(Duration::from_secs(2)).await.drained);
    let events = server.events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["event_type"] == "probe_event")
            .count(),
        1
    );
    assert!(!serde_json::to_string(&*events).unwrap().contains(&signed));
}

#[cfg(feature = "actix-web")]
#[actix_web::test]
async fn actix_automatically_binds_header_trigger_and_never_reuses_it() {
    use actix_web::{test, web, App, HttpMessage, HttpRequest};
    let server = Server::new(config());
    let client = server.client();
    wait(&client);
    let handler_client = client.clone();
    let app = test::init_service(
        App::new()
            .wrap(debugbundle::actix_web::DebugBundleMiddleware::new(
                client.clone(),
            ))
            .route(
                "/probe",
                web::get().to(move |request: HttpRequest| {
                    let client = handler_client.clone();
                    async move {
                        let context = request.extensions().get::<Context>().unwrap().clone();
                        client.probe(
                            "checkout.stage",
                            &json!(1),
                            ProbeOptions { heavy: true },
                            &context,
                        );
                        "ok"
                    }
                }),
            ),
    )
    .await;
    let signed = token("synthetic-signing-key", "checkout.*", "checkout", 60);
    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/probe")
            .insert_header(("X-DebugBundle-Probe-Trigger", signed.as_str()))
            .to_request(),
    )
    .await;
    drop(response);
    let response =
        test::call_service(&app, test::TestRequest::get().uri("/probe").to_request()).await;
    drop(response);
    assert!(client.flush_async(Duration::from_secs(2)).await.drained);
    let events = server.events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["event_type"] == "probe_event")
            .count(),
        1
    );
    assert!(!serde_json::to_string(&*events).unwrap().contains(&signed));
}

#[test]
fn free_configuration_stops_polling_and_never_arms_trigger_callbacks() {
    let server = Server::new(json!({"probes_enabled":true,"remote_probes_enabled":false}));
    let client = server.client();
    wait(&client);
    assert!(server.requests.lock().unwrap()[0].contains("service=checkout&environment=test"));
    let operation = client.start_operation(OperationOptions {
        kind: OperationKind::Request,
        ..Default::default()
    });
    assert!(!client.bind_probe_trigger(
        &operation.handle(),
        Some(&token("synthetic-signing-key", "*", "checkout", 60)),
        None
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let producer = Arc::new(move || {
        observed.fetch_add(1, Ordering::SeqCst);
        Value::Null
    });
    client.probe_lazy(
        "heavy",
        &producer,
        ProbeOptions { heavy: true },
        &operation.context(),
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn signed_triggers_are_request_scoped_and_revoked_by_key_rotation() {
    let server = Server::new(config());
    let client = server.client();
    wait(&client);
    let operation = client.start_operation(OperationOptions {
        kind: OperationKind::Request,
        ..Default::default()
    });
    let signed = token("synthetic-signing-key", "checkout.*", "checkout", 60);
    assert!(client.bind_probe_trigger(&operation.handle(), Some(&signed), None));
    let producer = Arc::new(|| json!({"technical":true}));
    client.probe_lazy(
        "checkout.stage",
        &producer,
        ProbeOptions { heavy: true },
        &operation.context(),
    );
    assert!(client.flush_async(Duration::from_secs(2)).await.drained);
    let other = client.start_operation(OperationOptions {
        kind: OperationKind::Request,
        ..Default::default()
    });
    client.probe(
        "checkout.other",
        &json!(true),
        ProbeOptions { heavy: true },
        &other.context(),
    );
    server.config.lock().unwrap()["trigger_token_key"] = json!("rotated");
    assert!(
        client
            .refresh_remote_config_async(Duration::from_secs(2))
            .await
    );
    client.probe(
        "checkout.revoked",
        &json!(true),
        ProbeOptions { heavy: true },
        &operation.context(),
    );
    assert!(client.flush_async(Duration::from_secs(2)).await.drained);
    let events = server.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["payload"]["label"], "checkout.stage");
    assert!(!serde_json::to_string(&*events).unwrap().contains(&signed));
}

#[test]
fn invalid_header_never_falls_back_to_query_and_unsigned_scopes_are_rejected() {
    let server = Server::new(config());
    let client = server.client();
    wait(&client);
    let operation = client.start_operation(OperationOptions {
        kind: OperationKind::Request,
        ..Default::default()
    });
    let good = token("synthetic-signing-key", "*", "checkout", 60);
    assert!(!client.bind_probe_trigger(&operation.handle(), Some("invalid"), Some(&good)));
    for invalid in [
        token("wrong", "*", "checkout", 60),
        token("synthetic-signing-key", "*", "another", 60),
        token("synthetic-signing-key", "*", "checkout", -1),
        token("synthetic-signing-key", "*", "checkout", 86410),
    ] {
        assert!(!client.bind_probe_trigger(&operation.handle(), Some(&invalid), None));
    }
    assert!(client.bind_probe_trigger(&operation.handle(), None, Some(&good)));
    let job = client.start_operation(OperationOptions::default());
    assert!(!client.bind_probe_trigger(&job.handle(), Some(&good), None));
}

#[test]
fn passive_directives_with_one_activation_id_keep_distinct_patterns() {
    let mut body = config();
    let expires = (chrono::Utc::now() + chrono::Duration::seconds(60))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    body["active_probes"] = json!([{"id":"11111111-1111-4111-8111-111111111111","label_pattern":"checkout.*","service":"checkout","environment":"test","expires_at":expires},{"activation_id":"11111111-1111-4111-8111-111111111111","label_pattern":"*","service":"checkout","environment":"test","expires_at":expires}]);
    let server = Server::new(body);
    let client = server.client();
    wait(&client);
    client.probe(
        "checkout.stage",
        &json!(1),
        ProbeOptions { heavy: true },
        &Context::default(),
    );
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert_eq!(server.events.lock().unwrap().len(), 2);
}
