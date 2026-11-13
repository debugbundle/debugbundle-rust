use axum::{
    extract::{Extension, State},
    http::StatusCode,
    routing::get,
    Router,
};
use debugbundle::{
    relay::{Relay, RelayOptions},
    Client, Context, ExceptionSnapshot,
};
use std::time::Duration;
async fn failure(
    State(client): State<Client>,
    Extension(context): Extension<Context>,
) -> (StatusCode, &'static str) {
    client.capture_exception(
        &ExceptionSnapshot::new("AxumFailure", "synthetic application failure"),
        &context,
    );
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "unchanged application body",
    )
}
#[tokio::main]
async fn main() {
    let mock = artifact_support::Mock::start();
    let directory = tempfile::tempdir().unwrap();
    let client = mock.client("artifact-axum", directory.path().join("spool"));
    mock.wait_config(&client);
    let relay = Relay::new(client.clone(), RelayOptions::default());
    let app = Router::new()
        .route("/failure/{id}", get(failure))
        .with_state(client.clone())
        .merge(debugbundle::axum::browser_relay(relay.clone()))
        .layer(debugbundle::axum::DebugBundleLayer::new(client.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let response = http
        .get(format!("{origin}/failure/42"))
        .header("X-DebugBundle-Trace-Id", artifact_support::TRACE)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 500);
    assert_eq!(
        response.headers()["x-debugbundle-trace-id"],
        artifact_support::TRACE
    );
    assert_eq!(response.text().await.unwrap(), "unchanged application body");
    let response = http
        .post(format!(
            "{origin}/debugbundle/browser?debugbundle_evidence=1"
        ))
        .header("origin", &origin)
        .json(&artifact_support::browser_batch())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["accepted"],
        1
    );
    assert!(client.flush_async(Duration::from_secs(5)).await.drained);
    assert!(relay.flush_async(Duration::from_secs(5)).await);
    mock.assert_framework("axum");
    assert!(client.shutdown_async(Duration::from_secs(1)).await.drained);
    server.abort();
    let _ = server.await;
    println!("Axum exact-artifact TCP/middleware/durable-relay acceptance passed");
}
