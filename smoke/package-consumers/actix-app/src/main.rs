use actix_web::{web, App, HttpMessage, HttpRequest, HttpResponse, HttpServer};
use debugbundle::{
    relay::{Relay, RelayOptions},
    Client, Context, ExceptionSnapshot,
};
use std::{cell::Cell, rc::Rc, time::Duration};
async fn failure(
    client: web::Data<Client>,
    request: HttpRequest,
    local: web::Data<Rc<Cell<usize>>>,
) -> HttpResponse {
    local.set(local.get() + 1);
    let context = request.extensions().get::<Context>().unwrap().clone();
    client.capture_exception(
        &ExceptionSnapshot::new("ActixFailure", "synthetic application failure"),
        &context,
    );
    HttpResponse::InternalServerError().body("unchanged application body")
}
#[actix_web::main]
async fn main() {
    let mock = artifact_support::Mock::start();
    let directory = tempfile::tempdir().unwrap();
    let client = mock.client("artifact-actix", directory.path().join("spool"));
    mock.wait_config(&client);
    let relay = Relay::new(client.clone(), RelayOptions::default());
    let server_client = client.clone();
    let server_relay = relay.clone();
    let server = HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(server_client.clone()))
            .app_data(web::Data::new(Rc::new(Cell::new(0usize))))
            .wrap(debugbundle::actix_web::DebugBundleMiddleware::new(
                server_client.clone(),
            ))
            .route("/failure/{id}", web::get().to(failure))
            .service(debugbundle::actix_web::browser_relay(server_relay.clone()))
    })
    .workers(1)
    .bind(("127.0.0.1", 0))
    .unwrap();
    let origin = format!("http://{}", server.addrs()[0]);
    let server = server.run();
    let handle = server.handle();
    let running = actix_web::rt::spawn(server);
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
    mock.assert_framework("actix-web");
    assert!(client.shutdown_async(Duration::from_secs(1)).await.drained);
    drop(http);
    handle.stop(true).await;
    running.await.unwrap().unwrap();
    println!("Actix exact-artifact TCP/non-Send/middleware/durable-relay acceptance passed");
}
