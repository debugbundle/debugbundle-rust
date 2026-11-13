use debugbundle::{Client, Config};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Empty, Full};
use std::{convert::Infallible, time::Duration};
use tower::{Layer, ServiceExt};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(Config::local("example-tower", ".debugbundle/local/events"));
    let service = tower::service_fn(|_: Request<Empty<&'static [u8]>>| async {
        let mut response = Response::new(Full::new(&b"synthetic failure"[..]));
        *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
        Ok::<_, Infallible>(response)
    });
    let service = debugbundle::tower::DebugBundleLayer::new(client.clone()).layer(service);
    let response = service.oneshot(Request::new(Empty::new())).await?;
    let _body = response.into_body().collect().await?;
    client.shutdown_async(Duration::from_secs(2)).await;
    Ok(())
}
