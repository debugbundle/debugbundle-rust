use crate::relay::{native, Relay, RelayConnection, RelayResponse};
use ::actix_web::{dev::HttpServiceFactory, web, HttpMessage, HttpRequest, HttpResponse};
use futures_util::StreamExt;

/// Mount the browser relay without Actix's eager body extractors. Socket/TLS facts
/// come from the native listener, never `connection_info()` or forwarded headers.
pub fn browser_relay(relay: Relay) -> impl HttpServiceFactory {
    web::resource("/debugbundle/browser")
        .app_data(web::Data::new(relay))
        .to(handle)
}
async fn handle(
    relay: web::Data<Relay>,
    request: HttpRequest,
    mut body: web::Payload,
) -> HttpResponse {
    let connection = request
        .extensions()
        .get::<RelayConnection>()
        .copied()
        .unwrap_or_else(|| RelayConnection {
            secure: request.app_config().secure(),
            peer: request.peer_addr().map(|peer| peer.ip()),
        });
    let head = native::head(
        request.method().as_str(),
        connection,
        request.uri().query(),
        |name| {
            let mut values = request.headers().get_all(name);
            let first = values.next();
            if values.next().is_some() {
                return Err(());
            }
            first
                .map(|value| value.to_str().map_err(|_| ()))
                .transpose()
        },
    );
    let (head, declared) = match head {
        Ok(head) => head,
        Err(error) => return response(error),
    };
    let admission = match relay.begin(&head, declared).await {
        Ok(admission) => admission,
        Err(error) => return response(error),
    };
    let read = async {
        let mut bytes = Vec::new();
        let mut frames = 0;
        while let Some(chunk) = body.next().await {
            frames += 1;
            if frames > native::MAX_FRAMES {
                return Err(413);
            }
            let chunk = chunk.map_err(|_| 400u16)?;
            if chunk.len() > crate::wire::MAX_BODY_BYTES.saturating_sub(bytes.len()) {
                return Err(413);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    };
    response(
        match tokio::time::timeout(native::BODY_TIMEOUT, read).await {
            Ok(Ok(bytes)) => admission.finish(&bytes).await,
            Ok(Err(status)) => admission.reject(status),
            Err(_) => admission.reject(408),
        },
    )
}
fn response(response: RelayResponse) -> HttpResponse {
    let mut native = HttpResponse::build(
        ::actix_web::http::StatusCode::from_u16(response.status)
            .unwrap_or(::actix_web::http::StatusCode::INTERNAL_SERVER_ERROR),
    );
    for (key, value) in response.headers {
        native.insert_header((key, value));
    }
    match response.body {
        Some(body) => native.json(body),
        None => native.finish(),
    }
}
