use crate::{measure::Measure, support::*};
use debugbundle::{Client, Context, ExceptionSnapshot};
use serde_json::Value;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tower::ServiceExt;

#[derive(Clone)]
struct AxumState {
    client: Client,
    values: Arc<Vec<ExceptionSnapshot>>,
    next: Arc<AtomicUsize>,
}
async fn axum_failure(
    axum::extract::State(state): axum::extract::State<AxumState>,
    axum::extract::Extension(context): axum::extract::Extension<Context>,
) -> axum::http::StatusCode {
    let index = state.next.fetch_add(1, Ordering::Relaxed);
    state
        .client
        .capture_exception(&state.values[index], &context);
    axum::http::StatusCode::INTERNAL_SERVER_ERROR
}
pub fn run(mode: &str) -> Vec<Value> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let client = Client::new(config(mode, sink(None)));
    let state = AxumState {
        client: client.clone(),
        values: Arc::new(snapshots(65)),
        next: Arc::new(AtomicUsize::new(0)),
    };
    let app = axum::Router::new()
        .route("/failure/{id}", axum::routing::get(axum_failure))
        .with_state(state)
        .layer(debugbundle::axum::DebugBundleLayer::new(client.clone()));
    let mut measure = Measure::new(format!("axum/{mode}"), 64);
    for index in 0..65 {
        let request = axum::http::Request::builder()
            .uri(format!("/failure/{index}"))
            .body(axum::body::Body::empty())
            .unwrap();
        let call = || {
            runtime.block_on(async {
                let response = app.clone().oneshot(request).await.unwrap();
                assert_eq!(response.status(), 500);
                let _ = axum::body::to_bytes(response.into_body(), 8192)
                    .await
                    .unwrap();
            })
        };
        if index == 0 {
            call();
        } else {
            measure.call(call);
        }
        measure.observe(&client);
        drain(&client);
    }
    client.shutdown(Duration::from_secs(2));
    vec![measure.finish(), actix(mode)]
}
fn actix(mode: &str) -> Value {
    use actix_web::{test, web, App, HttpMessage, HttpRequest, HttpResponse};
    use std::{cell::Cell, rc::Rc};
    struct State {
        client: Client,
        values: Vec<ExceptionSnapshot>,
        next: Cell<usize>,
    }
    async fn failure(request: HttpRequest, state: web::Data<State>) -> HttpResponse {
        let index = state.next.get();
        state.next.set(index + 1);
        let context = request
            .extensions()
            .get::<Context>()
            .cloned()
            .unwrap_or_default();
        state
            .client
            .capture_exception(&state.values[index], &context);
        HttpResponse::InternalServerError().finish()
    }
    let runtime = actix_web::rt::System::new();
    let client = Client::new(config(mode, sink(None)));
    let state = web::Data::new(State {
        client: client.clone(),
        values: snapshots(65),
        next: Cell::new(0),
    });
    // Keep genuine local state in this application; no Send bound is introduced.
    let local = Rc::new(Cell::new(0usize));
    let state_counter = local.clone();
    let app = runtime.block_on(test::init_service(
        App::new()
            .app_data(state)
            .app_data(web::Data::new(state_counter))
            .wrap(debugbundle::actix_web::DebugBundleMiddleware::new(
                client.clone(),
            ))
            .route("/failure/{id}", web::get().to(failure)),
    ));
    let mut measure = Measure::new(format!("actix/{mode}"), 64);
    for index in 0..65 {
        let request = test::TestRequest::get()
            .uri(&format!("/failure/{index}"))
            .to_request();
        let call = || {
            runtime.block_on(async {
                let response = test::call_service(&app, request).await;
                assert_eq!(response.status(), 500);
                let _ = test::read_body(response).await;
            })
        };
        if index == 0 {
            call();
        } else {
            measure.call(call);
        }
        measure.observe(&client);
        drain(&client);
    }
    client.shutdown(Duration::from_secs(2));
    measure.finish()
}
