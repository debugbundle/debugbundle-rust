use crate::relay::{native, Relay, RelayConnection, RelayResponse};
use ::axum::{
    body::Body,
    extract::{ConnectInfo, Request, State},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use std::{net::SocketAddr, pin::Pin};

/// Mount the same-origin browser relay at `/debugbundle/browser`.
/// Use `into_make_service_with_connect_info::<SocketAddr>()` for per-peer limits;
/// custom TLS listeners can supply verified `RelayConnection` extensions.
pub fn browser_relay(relay: Relay) -> Router {
    Router::new()
        .route("/debugbundle/browser", any(handle))
        .with_state(relay)
}

async fn handle(State(relay): State<Relay>, request: Request) -> Response {
    let (parts, mut body) = request.into_parts();
    let connection = parts
        .extensions
        .get::<RelayConnection>()
        .copied()
        .unwrap_or_else(|| RelayConnection {
            secure: false,
            peer: parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|peer| peer.0.ip()),
        });
    let head = native::head(
        parts.method.as_str(),
        connection,
        parts.uri.query(),
        |name| {
            let mut values = parts.headers.get_all(name).iter();
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
        while let Some(frame) =
            std::future::poll_fn(|cx| http_body::Body::poll_frame(Pin::new(&mut body), cx)).await
        {
            frames += 1;
            if frames > native::MAX_FRAMES {
                return Err(413);
            }
            let frame = frame.map_err(|_| 400u16)?;
            if let Ok(data) = frame.into_data() {
                if data.len() > crate::wire::MAX_BODY_BYTES.saturating_sub(bytes.len()) {
                    return Err(413);
                }
                bytes.extend_from_slice(&data);
            }
        }
        Ok(bytes)
    };
    let result = match tokio::time::timeout(native::BODY_TIMEOUT, read).await {
        Ok(Ok(bytes)) => admission.finish(&bytes).await,
        Ok(Err(status)) => admission.reject(status),
        Err(_) => admission.reject(408),
    };
    response(result)
}
fn response(response: RelayResponse) -> Response {
    let mut native = match response.body {
        Some(body) => Json(body).into_response(),
        None => Body::empty().into_response(),
    };
    *native.status_mut() = ::axum::http::StatusCode::from_u16(response.status)
        .unwrap_or(::axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    for (key, value) in response.headers {
        if let (Ok(key), Ok(value)) = (
            ::axum::http::HeaderName::try_from(key),
            ::axum::http::HeaderValue::try_from(value),
        ) {
            native.headers_mut().insert(key, value);
        }
    }
    native
}
