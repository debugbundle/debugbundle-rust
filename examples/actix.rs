use actix_web::{web, App, HttpResponse, HttpServer};
use debugbundle::{
    relay::{Relay, RelayOptions},
    Client, Config,
};
use std::time::Duration;
use tracing_subscriber::prelude::*;

#[actix_web::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(Config::from_env());
    tracing_subscriber::registry()
        .with(client.tracing_layer())
        .try_init()?;
    // Clone one relay into worker factories; all instances for this client also
    // share admission and rate accounting if constructed independently.
    let relay = Relay::new(client.clone(), RelayOptions::default());
    let app_client = client.clone();
    HttpServer::new(move || {
        App::new()
            .wrap(debugbundle::actix_web::DebugBundleMiddleware::new(
                app_client.clone(),
            ))
            .service(debugbundle::actix_web::browser_relay(relay.clone()))
            .route(
                "/failure/{id}",
                web::get().to(|| async { HttpResponse::InternalServerError().finish() }),
            )
    })
    .bind(("127.0.0.1", 3000))?
    .run()
    .await?;
    client.shutdown_async(Duration::from_secs(2)).await;
    Ok(())
}
