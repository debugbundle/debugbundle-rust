#![cfg(any(feature = "axum", feature = "actix-web"))]
mod helpers;
use debugbundle::{Client, Context, RequestBodySnapshot};
use helpers::{configured, Recorder};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

#[cfg(feature = "axum")]
#[tokio::test]
async fn axum_inactive_context_drop_and_panic_do_not_change_native_behaviour() {
    use axum::{
        body::Body,
        extract::Extension,
        http::{Request, StatusCode},
        routing::get,
        Router,
    };
    use tower::ServiceExt;
    let recorder = Arc::new(Recorder::default());
    let mut config = configured(recorder);
    config.enabled = false;
    let disabled = Client::new(config);
    let app = Router::new()
        .route(
            "/",
            get(|Extension(context): Extension<Context>| async move {
                assert!(!context.operation_active());
                StatusCode::OK
            }),
        )
        .layer(debugbundle::axum::DebugBundleLayer::new(disabled));
    assert_eq!(
        app.oneshot(Request::new(Body::empty()))
            .await
            .unwrap()
            .status(),
        200
    );
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    let observed = Arc::new(Mutex::new(None::<Context>));
    let captured = observed.clone();
    let app = Router::new()
        .route(
            "/",
            get(move |Extension(context): Extension<Context>| {
                let captured = captured.clone();
                async move {
                    *captured.lock().unwrap() = Some(context);
                    (StatusCode::SERVICE_UNAVAILABLE, "stream not consumed")
                }
            }),
        )
        .layer(debugbundle::axum::DebugBundleLayer::new(client.clone()));
    let response = app
        .oneshot(
            Request::builder()
                .header("X-DebugBundle-Trace-Id", "invalid")
                .header("X-Request-Id", "password=SYNTHETIC_SECRET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let generated = response.headers()["X-DebugBundle-Trace-Id"]
        .to_str()
        .unwrap();
    assert!(uuid::Uuid::parse_str(generated).is_ok());
    assert!(observed
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .operation_active());
    drop(response);
    assert!(!observed
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .operation_active());
    let captured = observed.clone();
    let app = Router::new()
        .route(
            "/",
            get(move |Extension(context): Extension<Context>| {
                let captured = captured.clone();
                async move {
                    *captured.lock().unwrap() = Some(context);
                    panic!("application handler panic");
                    #[allow(unreachable_code)]
                    StatusCode::OK
                }
            }),
        )
        .layer(debugbundle::axum::DebugBundleLayer::new(client.clone()));
    let failure = tokio::spawn(app.oneshot(Request::new(Body::empty()))).await;
    assert!(failure.unwrap_err().is_panic());
    assert!(!observed
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .operation_active());
    assert!(client.flush_async(Duration::from_secs(2)).await.drained);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event_type"], "request_event");
    assert!(events[0]["payload"]["body"].is_null());
    assert!(
        uuid::Uuid::parse_str(events[0]["correlation"]["request_id"].as_str().unwrap()).is_ok()
    );
}

#[cfg(feature = "actix-web")]
#[actix_web::test]
async fn actix_body_opt_in_and_typed_error_preserve_native_conversion() {
    use actix_web::{
        http::StatusCode, test, web, App, HttpMessage, HttpRequest, HttpResponse, ResponseError,
    };
    use debugbundle::{ExceptionOccurrence, ExceptionSnapshot};
    #[derive(Debug)]
    struct NativeFailure;
    impl std::fmt::Display for NativeFailure {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("native conversion")
        }
    }
    impl ResponseError for NativeFailure {
        fn status_code(&self) -> StatusCode {
            StatusCode::BAD_GATEWAY
        }
        fn error_response(&self) -> HttpResponse {
            HttpResponse::BadGateway().body("preserved native failure")
        }
    }
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    let handler_client = client.clone();
    let app = test::init_service(
        App::new()
            .wrap(
                debugbundle::actix_web::DebugBundleMiddleware::new(client.clone()).with_options(
                    debugbundle::HttpOptions {
                        framework_version: Some("4.19.0".into()),
                        excluded_paths: vec![
                            "/health".into(),
                            "invalid?password=SYNTHETIC_SECRET".into(),
                        ],
                    },
                ),
            )
            .route(
                "/typed",
                web::post().to(move |request: HttpRequest, body: web::Bytes| {
                    let client = handler_client.clone();
                    async move {
                        assert_eq!(&body[..], b"untouched stream");
                        let context = request.extensions().get::<Context>().unwrap().clone();
                        Err::<HttpResponse, _>(debugbundle::actix_web::ReportedError::new(
                            NativeFailure,
                            client,
                            ExceptionOccurrence::new(&ExceptionSnapshot::new(
                                "NativeFailure",
                                "safe snapshot",
                            ))
                            .unwrap(),
                            context,
                        ))
                    }
                }),
            ),
    )
    .await;
    let request = test::TestRequest::post()
        .uri("/typed")
        .set_payload("untouched stream")
        .to_request();
    request.extensions_mut().insert(
        RequestBodySnapshot::new(&json!({"api_key":"SYNTHETIC_SECRET","technical":7})).unwrap(),
    );
    let response = test::call_service(&app, request).await;
    assert_eq!(response.status(), 502);
    assert_eq!(
        &test::read_body(response).await[..],
        b"preserved native failure"
    );
    assert!(client.flush_async(Duration::from_secs(2)).await.drained);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 2);
    let exception = events
        .iter()
        .find(|event| event["event_type"] == "backend_exception")
        .unwrap();
    assert_eq!(
        exception["payload"]["request"]["body"],
        json!({"api_key":"[REDACTED]","technical":7})
    );
    assert_eq!(exception["payload"]["response"]["status_code"], 502);
    assert_eq!(
        exception["payload"]["runtime"]["framework_version"],
        "4.19.0"
    );
    assert!(!serde_json::to_string(&*events)
        .unwrap()
        .contains("SYNTHETIC_SECRET"));
}

#[cfg(feature = "axum")]
#[tokio::test]
async fn axum_preserves_trace_and_streams_while_using_real_request_status() {
    use axum::{
        body::Body,
        extract::Extension,
        http::{Request, StatusCode},
        routing::get,
        Router,
    };
    use tower::ServiceExt;
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    let captured = Arc::new(Mutex::new(None::<Context>));
    let observed = captured.clone();
    let app = Router::new()
        .route(
            "/failure/{id}",
            get(move |Extension(context): Extension<Context>| {
                let observed = observed.clone();
                async move {
                    *observed.lock().unwrap() = Some(context);
                    (StatusCode::SERVICE_UNAVAILABLE, "unchanged body")
                }
            }),
        )
        .layer(debugbundle::axum::DebugBundleLayer::new(client.clone()));
    let trace = "11111111-1111-4111-8111-111111111111";
    let response = app
        .oneshot(
            Request::builder()
                .uri("/failure/42?token=SYNTHETIC_SECRET")
                .header("X-DebugBundle-Trace-Id", trace)
                .header("X-Request-Id", "req-checkout-42")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    assert_eq!(response.headers()["X-DebugBundle-Trace-Id"], trace);
    assert!(captured
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .operation_active());
    let bytes = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"unchanged body");
    assert!(!captured
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .operation_active());
    assert!(client.flush_async(Duration::from_secs(2)).await.drained);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event_type"], "request_event");
    assert_eq!(events[0]["payload"]["response_status"], 503);
    assert_eq!(events[0]["payload"]["route_template"], "/failure/{id}");
    assert_eq!(events[0]["service"]["framework"], "axum");
    assert_eq!(events[0]["correlation"]["request_id"], "req-checkout-42");
    assert!(!serde_json::to_string(&*events)
        .unwrap()
        .contains("SYNTHETIC_SECRET"));
}

#[cfg(feature = "axum")]
#[tokio::test]
async fn axum_body_snapshots_are_explicit_protected_and_do_not_consume_the_stream() {
    use axum::{
        body::{Body, Bytes},
        http::{Request, StatusCode},
        routing::post,
        Router,
    };
    use tower::ServiceExt;
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    let app = Router::new()
        .route(
            "/body",
            post(|bytes: Bytes| async move { (StatusCode::INTERNAL_SERVER_ERROR, bytes) }),
        )
        .layer(debugbundle::axum::DebugBundleLayer::new(client.clone()));
    let mut request = Request::builder()
        .method("POST")
        .uri("/body")
        .body(Body::from("original stream"))
        .unwrap();
    request.extensions_mut().insert(
        RequestBodySnapshot::new(&json!({"password":"SYNTHETIC_SECRET","technical":true})).unwrap(),
    );
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(
        &axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap()[..],
        b"original stream"
    );
    assert!(client.flush_async(Duration::from_secs(2)).await.drained);
    let events = recorder.events.lock().unwrap();
    assert_eq!(
        events[0]["payload"]["body"],
        json!({"password":"[REDACTED]","technical":true})
    );
    assert!(RequestBodySnapshot::new(&json!({"large":"x".repeat(10000)})).is_none());
}

#[cfg(feature = "actix-web")]
#[actix_web::test]
async fn actix_preserves_non_send_application_state_and_native_trace_response() {
    use actix_web::{test, web, App, HttpMessage, HttpRequest, HttpResponse};
    use std::{cell::Cell, rc::Rc};
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    let local = Rc::new(Cell::new(0usize));
    let observed = local.clone();
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(observed))
            .wrap(debugbundle::actix_web::DebugBundleMiddleware::new(
                client.clone(),
            ))
            .route(
                "/failure/{id}",
                web::get().to(
                    |request: HttpRequest, state: web::Data<Rc<Cell<usize>>>| async move {
                        state.set(state.get() + 1);
                        assert!(request
                            .extensions()
                            .get::<Context>()
                            .unwrap()
                            .operation_active());
                        HttpResponse::InternalServerError().body("unchanged body")
                    },
                ),
            ),
    )
    .await;
    let trace = "22222222-2222-4222-8222-222222222222";
    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/failure/42")
            .insert_header(("X-DebugBundle-Trace-Id", trace))
            .insert_header(("X-Request-Id", "req-actix-42"))
            .to_request(),
    )
    .await;
    assert_eq!(response.status(), 500);
    assert_eq!(
        response.headers().get("X-DebugBundle-Trace-Id").unwrap(),
        trace
    );
    assert_eq!(&test::read_body(response).await[..], b"unchanged body");
    assert_eq!(local.get(), 1);
    assert!(client.flush_async(Duration::from_secs(2)).await.drained);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event_type"], "request_event");
    assert_eq!(events[0]["payload"]["route_template"], "/failure/{id}");
    assert_eq!(events[0]["service"]["framework"], "actix-web");
    assert_eq!(events[0]["correlation"]["request_id"], "req-actix-42");
}
