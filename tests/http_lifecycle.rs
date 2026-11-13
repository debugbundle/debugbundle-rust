#![cfg(feature = "axum")]
mod helpers;
use debugbundle::{Client, Context, ExceptionOccurrence, ExceptionSnapshot, OperationOptions};
use helpers::{configured, Recorder};
use http::{Request, Response};
use http_body_util::{BodyExt, Full};
use std::{
    cell::Cell,
    future::{ready, Future, Ready},
    pin::Pin,
    rc::Rc,
    sync::{Arc, Mutex},
    task::{Context as TaskContext, Poll},
    time::Duration,
};
use tower::{Layer, Service, ServiceExt};
type Bytes = axum::body::Bytes;

struct PendingService {
    ready: Rc<Cell<bool>>,
    context: Arc<Mutex<Option<Context>>>,
    client: Client,
}
impl Service<Request<()>> for PendingService {
    type Response = Response<Full<Bytes>>;
    type Error = std::convert::Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;
    fn poll_ready(&mut self, _: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.ready.set(true);
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, request: Request<()>) -> Self::Future {
        assert!(self.ready.replace(false));
        let context = request.extensions().get::<Context>().unwrap().clone();
        assert!(context.operation_active());
        *self.context.lock().unwrap() = Some(context);
        self.client.capture_exception(
            &ExceptionSnapshot::new("SynchronousCall", "still inside the actual service call"),
            &Context::default(),
        );
        let local = self.ready.clone();
        Box::pin(async move {
            let _local = local;
            std::future::pending().await
        })
    }
}
#[tokio::test]
async fn readiness_uses_same_service_and_cancellation_revokes_without_a_completed_request() {
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    let observed = Arc::new(Mutex::new(None));
    let mut service =
        debugbundle::tower::DebugBundleLayer::new(client.clone()).layer(PendingService {
            ready: Rc::new(Cell::new(false)),
            context: observed.clone(),
            client: client.clone(),
        });
    service.ready().await.unwrap();
    let future = service.call(Request::new(()));
    assert!(observed
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .operation_active());
    drop(future);
    assert!(!observed
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .operation_active());
    assert!(client.flush_async(Duration::from_secs(1)).await.drained);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event_type"], "backend_exception");
    assert_eq!(events[0]["service"]["framework"], "tower");
    assert_eq!(events[0]["payload"]["request"]["method"], "GET");
    assert!(events[0]["correlation"]["trace_id"].is_string());
    assert_eq!(events[0]["payload"]["response"]["status_code"], 0);
}

struct BodyError;
impl std::fmt::Debug for BodyError {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        panic!("SDK must not debug-format native errors")
    }
}
impl std::fmt::Display for BodyError {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        panic!("SDK must not format native errors")
    }
}
impl std::error::Error for BodyError {}
struct BrokenBody;
impl http_body::Body for BrokenBody {
    type Data = Bytes;
    type Error = BodyError;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, BodyError>>> {
        Poll::Ready(Some(Err(BodyError)))
    }
}
struct BrokenService;
impl Service<Request<()>> for BrokenService {
    type Response = Response<BrokenBody>;
    type Error = std::convert::Infallible;
    type Future = Ready<Result<Self::Response, Self::Error>>;
    fn poll_ready(&mut self, _: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, _: Request<()>) -> Self::Future {
        ready(Ok(Response::new(BrokenBody)))
    }
}
#[tokio::test]
async fn native_body_error_is_returned_and_captured_without_formatting() {
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    let response = debugbundle::tower::DebugBundleLayer::new(client.clone())
        .layer(BrokenService)
        .oneshot(Request::new(()))
        .await
        .unwrap();
    assert!(response.into_body().collect().await.is_err());
    assert!(client.flush_async(Duration::from_secs(2)).await.drained);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event_type"], "backend_exception");
    assert_eq!(events[0]["payload"]["response"]["status_code"], 200);
    assert!(events[0]["payload"]["name"]
        .as_str()
        .unwrap()
        .ends_with("BodyError"));
}

#[test]
fn explicit_outbound_trace_is_first_party_only_and_excludes_sdk_transport() {
    let recorder = Arc::new(Recorder::default());
    let mut config = configured(recorder);
    config.trace_propagation_targets = vec![
        "https://first.example".into(),
        "https://api.debugbundle.com".into(),
    ];
    let client = Client::new(config);
    let trace = "33333333-3333-4333-8333-333333333333";
    let op = client.start_operation(OperationOptions {
        trace_id: Some(trace.into()),
        ..OperationOptions::default()
    });
    assert_eq!(
        client
            .trace_header_for("https://first.example/path", &op.context())
            .as_deref(),
        Some(trace)
    );
    for url in [
        "https://third.example/path",
        "https://first.example.attacker.test/path",
        "https://user:password@first.example/path",
        "https://api.debugbundle.com/v1/events",
        "https://api.debugbundle.com/v1/sdk/config",
    ] {
        assert!(
            client.trace_header_for(url, &op.context()).is_none(),
            "{url}"
        );
    }
}

#[cfg(feature = "axum")]
#[tokio::test]
async fn nested_layers_typed_errors_and_extractor_rejection_preserve_native_responses() {
    use axum::{
        extract::{Extension, Json},
        http::StatusCode,
        routing::{get, post},
        Router,
    };
    use serde_json::Value;
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    let handler_client = client.clone();
    let app = Router::new()
        .route(
            "/typed/{id}",
            get(move |Extension(context): Extension<Context>| {
                let client = handler_client.clone();
                async move {
                    debugbundle::axum::ErrorResponse::new(
                        (StatusCode::BAD_GATEWAY, "native error body"),
                        client,
                        ExceptionOccurrence::new(&ExceptionSnapshot::new(
                            "TypedFailure",
                            "safe technical message",
                        ))
                        .unwrap(),
                        context,
                    )
                }
            }),
        )
        .route(
            "/json",
            post(|Json(_value): Json<Value>| async { StatusCode::OK }),
        )
        .layer(debugbundle::axum::DebugBundleLayer::new(client.clone()))
        .layer(debugbundle::axum::DebugBundleLayer::new(client.clone()));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/typed/42?password=SYNTHETIC_SECRET")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 502);
    assert_eq!(
        &axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap()[..],
        b"native error body"
    );
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/json")
                .header("content-type", "application/json")
                .body(axum::body::Body::from("bad json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    drop(response);
    assert!(client.flush_async(Duration::from_secs(2)).await.drained);
    let events = recorder.events.lock().unwrap();
    assert_eq!(events.len(), 2);
    let error = events
        .iter()
        .find(|event| event["event_type"] == "backend_exception")
        .unwrap();
    assert_eq!(error["payload"]["request"]["method"], "GET");
    assert_eq!(error["payload"]["response"]["status_code"], 502);
    assert_eq!(
        error["payload"]["request"]["query"]["password"],
        "[REDACTED]"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["event_type"] == "request_event")
            .count(),
        1
    );
}
