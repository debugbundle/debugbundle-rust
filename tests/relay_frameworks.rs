#![cfg(all(feature = "axum", feature = "actix-web"))]
use debugbundle::{
    relay::{Relay, RelayConnection, RelayOptions},
    Client, Config,
};
use serde_json::{json, Value};

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/relay-compliance.json")).unwrap()
}
fn relay(directory: &std::path::Path) -> Relay {
    Relay::new(
        Client::new(Config::local("checkout", directory.join("events"))),
        RelayOptions::default(),
    )
}

#[tokio::test]
async fn axum_mount_preserves_shared_relay_handler_contracts() {
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;
    for case in fixture()["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["kind"] == "handler")
    {
        let directory = tempfile::tempdir().unwrap();
        let app = debugbundle::axum::browser_relay(relay(directory.path()));
        let input = &case["request"];
        let mut request = Request::builder()
            .method(input["method"].as_str().unwrap())
            .uri("/debugbundle/browser");
        for (key, value) in input["headers"].as_object().unwrap() {
            request = request.header(key, value.as_str().unwrap());
        }
        let body = if input.get("bodyGenerator").is_some() {
            vec![b'x'; 262145]
        } else {
            serde_json::to_vec(&input["bodyJson"]).unwrap()
        };
        let mut request = request.body(Body::from(body)).unwrap();
        request.extensions_mut().insert(RelayConnection {
            secure: true,
            peer: input["ipAddress"]
                .as_str()
                .and_then(|peer| peer.parse().ok()),
        });
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status().as_u16(),
            case["expected"]["status"].as_u64().unwrap() as u16,
            "{}",
            case["id"]
        );
        if let Some(accepted) = case["expected"].get("accepted") {
            let body: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap())
                    .unwrap();
            assert_eq!(&body["accepted"], accepted, "{}", case["id"]);
            assert_eq!(
                body["rejected"], case["expected"]["rejected"],
                "{}",
                case["id"]
            );
            assert_eq!(body["errors"], case["expected"]["errors"], "{}", case["id"]);
        }
    }
}

#[actix_web::test]
async fn actix_mount_preserves_shared_relay_handler_contracts() {
    use actix_web::{test, App, HttpMessage};
    for case in fixture()["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["kind"] == "handler")
    {
        let directory = tempfile::tempdir().unwrap();
        let app = test::init_service(App::new().service(debugbundle::actix_web::browser_relay(
            relay(directory.path()),
        )))
        .await;
        let input = &case["request"];
        let mut request = test::TestRequest::default()
            .method(
                actix_web::http::Method::from_bytes(input["method"].as_str().unwrap().as_bytes())
                    .unwrap(),
            )
            .uri("/debugbundle/browser")
            .peer_addr(
                format!("{}:1234", input["ipAddress"].as_str().unwrap())
                    .parse()
                    .unwrap(),
            );
        for (key, value) in input["headers"].as_object().unwrap() {
            request = request.insert_header((key.as_str(), value.as_str().unwrap()));
        }
        let body = if input.get("bodyGenerator").is_some() {
            vec![b'x'; 262145]
        } else {
            serde_json::to_vec(&input["bodyJson"]).unwrap()
        };
        let request = request.set_payload(body).to_request();
        request.extensions_mut().insert(RelayConnection {
            secure: true,
            peer: input["ipAddress"]
                .as_str()
                .and_then(|peer| peer.parse().ok()),
        });
        let response = test::call_service(&app, request).await;
        assert_eq!(
            response.status().as_u16(),
            case["expected"]["status"].as_u64().unwrap() as u16,
            "{}",
            case["id"]
        );
        if let Some(accepted) = case["expected"].get("accepted") {
            let body: Value = test::read_body_json(response).await;
            assert_eq!(&body["accepted"], accepted, "{}", case["id"]);
            assert_eq!(
                body["rejected"], case["expected"]["rejected"],
                "{}",
                case["id"]
            );
            assert_eq!(body["errors"], case["expected"]["errors"], "{}", case["id"]);
        }
    }
}

#[tokio::test]
async fn axum_slow_upload_capacity_is_released_on_cancellation_and_timeout() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let directory = tempfile::tempdir().unwrap();
    let app = debugbundle::axum::browser_relay(relay(directory.path()));
    let make_request = || {
        Request::builder()
            .method("POST")
            .uri("/debugbundle/browser")
            .header("host", "app.example.com")
            .header("origin", "http://app.example.com")
            .header("content-type", "application/json")
            .body(Body::from_stream(futures_util::stream::pending::<
                Result<axum::body::Bytes, std::io::Error>,
            >()))
            .unwrap()
    };
    let mut uploads = Vec::new();
    for _ in 0..8 {
        let mut future = Box::pin(app.clone().oneshot(make_request()));
        assert!(futures_util::poll!(future.as_mut()).is_pending());
        uploads.push(future);
    }
    assert_eq!(
        app.clone().oneshot(make_request()).await.unwrap().status(),
        503
    );
    drop(uploads);
    tokio::time::pause();
    let mut future = Box::pin(app.clone().oneshot(make_request()));
    assert!(futures_util::poll!(future.as_mut()).is_pending());
    tokio::time::advance(std::time::Duration::from_secs(6)).await;
    assert_eq!(future.await.unwrap().status(), 408);
    let request = Request::builder()
        .method("POST")
        .uri("/debugbundle/browser?debugbundle_evidence=1")
        .header("host", "app.example.com")
        .header("origin", "http://app.example.com")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"batch":[]}"#))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), 202);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(body.get("evidence_errors").is_some());
}

#[tokio::test]
async fn independently_constructed_relay_routes_share_the_clients_ingress_and_rate_budget() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let directory = tempfile::tempdir().unwrap();
    let client = Client::new(Config::local("checkout", directory.path().join("events")));
    let first =
        debugbundle::axum::browser_relay(Relay::new(client.clone(), RelayOptions::default()));
    let second =
        debugbundle::axum::browser_relay(Relay::new(client.clone(), RelayOptions::default()));
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/debugbundle/browser")
            .header("host", "app.example.com")
            .header("origin", "http://app.example.com")
            .header("content-type", "application/json")
            .body(Body::from_stream(futures_util::stream::pending::<
                Result<axum::body::Bytes, std::io::Error>,
            >()))
            .unwrap()
    };
    let mut uploads = Vec::new();
    for _ in 0..8 {
        let mut future = Box::pin(first.clone().oneshot(request()));
        assert!(futures_util::poll!(future.as_mut()).is_pending());
        uploads.push(future);
    }
    let mut overflow = Box::pin(second.oneshot(request()));
    let response = futures_util::poll!(overflow.as_mut());
    assert!(
        matches!(response, std::task::Poll::Ready(Ok(reply)) if reply.status() == 503),
        "a second route must reject before retaining another upload"
    );
    drop(uploads);

    let options = RelayOptions {
        rate_limit_per_minute: 1,
        ..Default::default()
    };
    // A different verified peer starts a fresh bucket, but constructing another
    // handler for this client must not reset that peer's rate count.
    let head = debugbundle::relay::RelayRequest {
        method: "POST".into(),
        scheme: "http".into(),
        host: "app.example.com".into(),
        origin: Some("http://app.example.com".into()),
        content_type: Some("application/json".into()),
        peer: Some("203.0.113.80".parse().unwrap()),
        ..Default::default()
    };
    let one = Relay::new(client.clone(), options.clone());
    let two = Relay::new(client, options);
    assert_eq!(one.handle(&head, br#"{"batch":[]}"#).await.status, 202);
    assert_eq!(two.handle(&head, br#"{"batch":[]}"#).await.status, 429);
}

#[actix_web::test]
async fn actix_preflight_method_errors_and_duplicate_origins_fail_closed() {
    use actix_web::{test, App};
    let directory = tempfile::tempdir().unwrap();
    let app = test::init_service(
        App::new().service(debugbundle::actix_web::browser_relay(relay(
            directory.path(),
        ))),
    )
    .await;
    for (method, status) in [("OPTIONS", 204), ("GET", 405), ("POST", 202)] {
        let request = test::TestRequest::default()
            .method(actix_web::http::Method::from_bytes(method.as_bytes()).unwrap())
            .uri("/debugbundle/browser?debugbundle_evidence=1")
            .insert_header(("host", "app.example.com"))
            .insert_header(("origin", "http://app.example.com"))
            .insert_header(("content-type", "application/json"))
            .set_payload(r#"{"batch":[]}"#)
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), status);
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .unwrap(),
            "http://app.example.com"
        );
    }
    let mut request = test::TestRequest::post()
        .uri("/debugbundle/browser")
        .insert_header(("host", "app.example.com"))
        .insert_header(("origin", "http://app.example.com"))
        .insert_header(("content-type", "application/json"))
        .to_request();
    request.headers_mut().append(
        actix_web::http::header::ORIGIN,
        actix_web::http::header::HeaderValue::from_static("http://app.example.com"),
    );
    assert_eq!(test::call_service(&app, request).await.status(), 400);
}

#[tokio::test]
async fn axum_origin_rejection_never_polls_body_and_preflight_does_not_need_a_body() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    struct NeverRead;
    impl http_body::Body for NeverRead {
        type Data = axum::body::Bytes;
        type Error = std::convert::Infallible;
        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
            panic!("rejected relay read the request body")
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let app = debugbundle::axum::browser_relay(relay(directory.path()));
    let request = Request::builder()
        .method("POST")
        .uri("/debugbundle/browser")
        .header("host", "app.example.com")
        .header("origin", "https://evil.example")
        .body(Body::new(NeverRead))
        .unwrap();
    assert_eq!(app.clone().oneshot(request).await.unwrap().status(), 403);
    let request = Request::builder()
        .method("OPTIONS")
        .uri("/debugbundle/browser")
        .header("host", "app.example.com")
        .header("origin", "http://app.example.com")
        .body(Body::new(NeverRead))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), 204);
    assert_eq!(
        response.headers()["access-control-allow-origin"],
        "http://app.example.com"
    );
}

#[tokio::test]
async fn axum_stream_bounds_and_forwarded_headers_are_not_implicitly_trusted() {
    use axum::{
        body::{Body, Bytes},
        http::Request,
    };
    use tower::ServiceExt;
    let directory = tempfile::tempdir().unwrap();
    let app = debugbundle::axum::browser_relay(relay(directory.path()));
    let request = Request::builder()
        .method("POST")
        .uri("/debugbundle/browser")
        .header("host", "internal.example")
        .header("origin", "https://app.example.com")
        .header("x-forwarded-host", "app.example.com")
        .header("x-forwarded-proto", "https")
        .header("content-type", "application/json")
        .body(Body::from(json!({"batch":[]}).to_string()))
        .unwrap();
    assert_eq!(app.clone().oneshot(request).await.unwrap().status(), 403);
    let stream = futures_util::stream::iter([
        Ok::<_, std::io::Error>(Bytes::from(vec![b'x'; 131073])),
        Ok(Bytes::from(vec![b'x'; 131073])),
    ]);
    let request = Request::builder()
        .method("POST")
        .uri("/debugbundle/browser")
        .header("host", "app.example.com")
        .header("origin", "http://app.example.com")
        .header("content-type", "application/json")
        .body(Body::from_stream(stream))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), 413);
    assert_eq!(
        response.headers()["access-control-allow-origin"],
        "http://app.example.com"
    );
}
