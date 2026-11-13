//! Actix instrumentation keeps non-Send services, futures and streams on their executor.
mod relay;
use crate::{
    http_context::RequestLifecycle, Client, Context, HttpOptions, RequestBodySnapshot,
    RequestSnapshot,
};
use ::actix_web::{
    body::{BodySize, MessageBody},
    dev::{Service, ServiceRequest, ServiceResponse, Transform},
    Error, HttpMessage,
};
use futures_util::future::{ready, LocalBoxFuture, Ready};
pub use relay::browser_relay;
use std::{
    pin::Pin,
    rc::Rc,
    task::{Context as TaskContext, Poll},
};

#[derive(Clone)]
pub struct DebugBundleMiddleware {
    client: Client,
    options: HttpOptions,
}
impl DebugBundleMiddleware {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            options: HttpOptions::default(),
        }
    }
    pub fn with_options(mut self, options: HttpOptions) -> Self {
        self.options = options.normalized();
        self
    }
}
impl<S, B> Transform<S, ServiceRequest> for DebugBundleMiddleware
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse<InstrumentedBody<B>>;
    type Error = Error;
    type InitError = ();
    type Transform = DebugBundleService<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;
    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(DebugBundleService {
            inner: Rc::new(service),
            middleware: self.clone(),
        }))
    }
}
pub struct DebugBundleService<S> {
    inner: Rc<S>,
    middleware: DebugBundleMiddleware,
}
impl<S, B> Service<ServiceRequest> for DebugBundleService<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse<InstrumentedBody<B>>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;
    fn poll_ready(&self, cx: &mut TaskContext<'_>) -> Poll<Result<(), Error>> {
        self.inner.poll_ready(cx)
    }
    fn call(&self, request: ServiceRequest) -> Self::Future {
        let nested = request
            .extensions()
            .get::<Context>()
            .is_some_and(|context| self.middleware.client.same_http_client(context));
        let lifecycle = if nested {
            None
        } else {
            let mut headers = serde_json::Map::new();
            let mut bytes = 0;
            for (key, value) in request.headers().iter().take(32) {
                if key.as_str() == "x-debugbundle-probe-trigger" {
                    continue;
                }
                let Ok(value) = value.to_str() else {
                    continue;
                };
                if key.as_str().len() > 128 || value.len() > 2048 {
                    continue;
                }
                bytes += key.as_str().len() + value.len();
                if bytes > 8192 {
                    break;
                }
                headers.insert(key.as_str().into(), serde_json::Value::String(value.into()));
            }
            let snapshot = RequestSnapshot {
                method: request.method().as_str().into(),
                path: crate::http_context::bounded_target(
                    request
                        .uri()
                        .path_and_query()
                        .map_or("/", |path| path.as_str()),
                ),
                route_template: request.match_pattern(),
                headers: serde_json::Value::Object(headers),
                body: request
                    .extensions()
                    .get::<RequestBodySnapshot>()
                    .map(|snapshot| snapshot.0.clone()),
                ..RequestSnapshot::default()
            };
            self.middleware.client.begin_http(
                snapshot,
                request
                    .headers()
                    .get("x-debugbundle-trace-id")
                    .and_then(|value| value.to_str().ok()),
                request
                    .headers()
                    .get("x-request-id")
                    .and_then(|value| value.to_str().ok()),
                "actix-web",
                &self.middleware.options,
            )
        };
        if let Some(scope) = &lifecycle {
            self.middleware.client.bind_http_trigger(
                &scope.owner.handle(),
                request
                    .headers()
                    .get("x-debugbundle-probe-trigger")
                    .map(|value| value.to_str().unwrap_or("")),
                request.uri().query(),
            );
            request.extensions_mut().insert(scope.context());
        } else if request.extensions().get::<Context>().is_none() {
            request.extensions_mut().insert(Context::default());
        }
        let future = if let Some(scope) = &lifecycle {
            scope.owner.handle().run(|| self.inner.call(request))
        } else {
            self.inner.call(request)
        };
        Box::pin(async move {
            let result = if let Some(scope) = &lifecycle {
                scope.owner.handle().wrap(future).await
            } else {
                future.await
            };
            match result {
                Err(error) => {
                    if let Some(scope) = lifecycle {
                        scope.error(&error);
                    }
                    Err(error)
                }
                Ok(mut response) => {
                    if let Some(scope) = &lifecycle {
                        scope.headers(
                            response.status().as_u16(),
                            response.request().match_pattern().as_deref(),
                        );
                        if let Some(value) = scope.trace().and_then(|trace| {
                            ::actix_web::http::header::HeaderValue::from_str(&trace).ok()
                        }) {
                            response.headers_mut().insert(
                                ::actix_web::http::header::HeaderName::from_static(
                                    "x-debugbundle-trace-id",
                                ),
                                value,
                            );
                        }
                    }
                    Ok(response.map_body(|_, body| InstrumentedBody {
                        inner: Box::pin(body),
                        lifecycle,
                    }))
                }
            }
        })
    }
}
pub struct InstrumentedBody<B> {
    inner: Pin<Box<B>>,
    lifecycle: Option<RequestLifecycle>,
}
impl<B: MessageBody> MessageBody for InstrumentedBody<B> {
    type Error = B::Error;
    fn size(&self) -> BodySize {
        self.inner.size()
    }
    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<::actix_web::web::Bytes, Self::Error>>> {
        let this = self.get_mut();
        let result = if let Some(scope) = &this.lifecycle {
            scope
                .owner
                .handle()
                .run(|| this.inner.as_mut().poll_next(cx))
        } else {
            this.inner.as_mut().poll_next(cx)
        };
        match &result {
            Poll::Ready(Some(Err(error))) => {
                if let Some(scope) = this.lifecycle.take() {
                    scope.error(error);
                }
            }
            Poll::Ready(None) => {
                this.lifecycle.take();
            }
            _ => {}
        }
        result
    }
}

/// Explicit exception evidence for native Actix errors converted inside handlers.
pub struct ReportedError<E> {
    error: E,
    client: Client,
    occurrence: crate::ExceptionOccurrence,
    context: Context,
}
impl<E> ReportedError<E> {
    pub fn new(
        error: E,
        client: Client,
        occurrence: crate::ExceptionOccurrence,
        context: Context,
    ) -> Self {
        Self {
            error,
            client,
            occurrence,
            context,
        }
    }
}
impl<E: std::fmt::Debug> std::fmt::Debug for ReportedError<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}
impl<E: std::fmt::Display> std::fmt::Display for ReportedError<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}
impl<E: ::actix_web::ResponseError> ::actix_web::ResponseError for ReportedError<E> {
    fn status_code(&self) -> ::actix_web::http::StatusCode {
        self.error.status_code()
    }
    fn error_response(&self) -> ::actix_web::HttpResponse {
        let response = self.error.error_response();
        self.client
            .http_response_status(&self.context, response.status().as_u16());
        self.client
            .capture_occurrence(&self.occurrence, &self.context);
        response
    }
}
