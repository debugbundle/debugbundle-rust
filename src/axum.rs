//! Axum's native routing and body types with the shared Tower lifecycle.
mod relay;
use crate::{Client, HttpOptions};
pub use relay::browser_relay;
#[derive(Clone)]
pub struct DebugBundleLayer(crate::tower::DebugBundleLayer);
impl DebugBundleLayer {
    pub fn new(client: Client) -> Self {
        Self(crate::tower::DebugBundleLayer::axum(client))
    }
    pub fn with_options(self, options: HttpOptions) -> Self {
        Self(self.0.with_options(options))
    }
}
impl<S> ::tower::Layer<S> for DebugBundleLayer {
    type Service = crate::tower::DebugBundleService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        ::tower::Layer::layer(&self.0, inner)
    }
}

/// An explicit safe snapshot for errors converted to a response inside a handler.
/// The application's native response conversion runs once and is preserved.
pub struct ErrorResponse<E> {
    error: E,
    client: Client,
    occurrence: crate::ExceptionOccurrence,
    context: crate::Context,
}
impl<E> ErrorResponse<E> {
    pub fn new(
        error: E,
        client: Client,
        occurrence: crate::ExceptionOccurrence,
        context: crate::Context,
    ) -> Self {
        Self {
            error,
            client,
            occurrence,
            context,
        }
    }
}
impl<E: ::axum::response::IntoResponse> ::axum::response::IntoResponse for ErrorResponse<E> {
    fn into_response(self) -> ::axum::response::Response {
        let response = self.error.into_response();
        self.client
            .http_response_status(&self.context, response.status().as_u16());
        self.client
            .capture_occurrence(&self.occurrence, &self.context);
        response
    }
}
