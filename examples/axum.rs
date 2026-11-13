use axum::{http::StatusCode, routing::get, Router};
use debugbundle::{
    relay::{Relay, RelayOptions},
    Client, Config,
};
use std::{net::SocketAddr, time::Duration};
use tracing_subscriber::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(Config::from_env());
    tracing_subscriber::registry()
        .with(client.tracing_layer())
        .try_init()?;
    let relay = Relay::new(client.clone(), RelayOptions::default());
    let app = Router::new()
        .route(
            "/failure/{id}",
            get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
        )
        .merge(debugbundle::axum::browser_relay(relay))
        .layer(debugbundle::axum::DebugBundleLayer::new(client.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    client.shutdown_async(Duration::from_secs(2)).await;
    Ok(())
}
