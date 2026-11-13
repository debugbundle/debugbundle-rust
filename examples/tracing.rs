use debugbundle::{Client, Config};
use std::time::Duration;
use tracing_subscriber::prelude::*;

fn main() {
    let client = Client::new(Config::local(
        "example-tracing",
        ".debugbundle/local/events",
    ));
    // Add this layer to the application's existing subscriber; global installation
    // is the application's choice. Other application layers keep their filtering.
    let subscriber = tracing_subscriber::registry().with(client.tracing_layer());
    tracing::subscriber::with_default(subscriber, || {
        tracing::error!(message = "synthetic job failed", attempt = 1u64);
    });
    client.shutdown(Duration::from_secs(2));
}
