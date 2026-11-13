//! Tower service instrumentation. Readiness and the actual service future are delegated.
use crate::{
    http_context::RequestLifecycle, Client, Context, HttpOptions, RequestBodySnapshot,
    RequestSnapshot,
};
use ::tower::{Layer, Service};
use http::{Request, Response};
use http_body::{Body, Frame, SizeHint};
use std::{
    future::Future,
    pin::Pin,
    task::{Context as TaskContext, Poll},
};

#[derive(Clone)]
pub struct DebugBundleLayer {
    client: Client,
    options: HttpOptions,
    framework: &'static str,
}
impl DebugBundleLayer {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            options: HttpOptions::default(),
            framework: "tower",
        }
    }
    pub fn with_options(mut self, options: HttpOptions) -> Self {
        self.options = options.normalized();
        self
    }
    #[cfg(feature = "axum")]
    pub(crate) fn axum(client: Client) -> Self {
        Self {
            framework: "axum",
            ..Self::new(client)
        }
    }
}
impl<S> Layer<S> for DebugBundleLayer {
    type Service = DebugBundleService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        DebugBundleService {
            inner,
            layer: self.clone(),
        }
    }
}
#[derive(Clone)]
pub struct DebugBundleService<S> {
    inner: S,
    layer: DebugBundleLayer,
}
impl<S, I, O> Service<Request<I>> for DebugBundleService<S>
where
    S: Service<Request<I>, Response = Response<O>>,
    O: Body,
{
    type Response = Response<InstrumentedBody<O>>;
    type Error = S::Error;
    type Future = ResponseFuture<S::Future>;
    fn poll_ready(&mut self, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }
    fn call(&mut self, mut request: Request<I>) -> Self::Future {
        let nested = request
            .extensions()
            .get::<Context>()
            .is_some_and(|context| self.layer.client.same_http_client(context));
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
            #[cfg(feature = "axum")]
            let route = request
                .extensions()
                .get::<::axum::extract::MatchedPath>()
                .map(|path| path.as_str().to_owned());
            #[cfg(not(feature = "axum"))]
            let route = None;
            let snapshot = RequestSnapshot {
                method: request.method().as_str().into(),
                path: crate::http_context::bounded_target(
                    request
                        .uri()
                        .path_and_query()
                        .map_or("/", |path| path.as_str()),
                ),
                route_template: route,
                headers: serde_json::Value::Object(headers),
                body: request
                    .extensions()
                    .get::<RequestBodySnapshot>()
                    .map(|snapshot| snapshot.0.clone()),
                ..RequestSnapshot::default()
            };
            self.layer.client.begin_http(
                snapshot,
                request
                    .headers()
                    .get("x-debugbundle-trace-id")
                    .and_then(|value| value.to_str().ok()),
                request
                    .headers()
                    .get("x-request-id")
                    .and_then(|value| value.to_str().ok()),
                self.layer.framework,
                &self.layer.options,
            )
        };
        if let Some(scope) = &lifecycle {
            self.layer.client.bind_http_trigger(
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
        ResponseFuture {
            future: Box::pin(future),
            lifecycle,
        }
    }
}
pub struct ResponseFuture<F> {
    future: Pin<Box<F>>,
    lifecycle: Option<RequestLifecycle>,
}
impl<F, B, E> Future for ResponseFuture<F>
where
    F: Future<Output = Result<Response<B>, E>>,
    B: Body,
{
    type Output = Result<Response<InstrumentedBody<B>>, E>;
    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let result = if let Some(scope) = &this.lifecycle {
            scope.owner.handle().run(|| this.future.as_mut().poll(cx))
        } else {
            this.future.as_mut().poll(cx)
        };
        match result {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => {
                if let Some(scope) = this.lifecycle.take() {
                    scope.error(&error);
                }
                Poll::Ready(Err(error))
            }
            Poll::Ready(Ok(mut response)) => {
                if let Some(scope) = &this.lifecycle {
                    scope.headers(response.status().as_u16(), None);
                    if let Some(value) = scope
                        .trace()
                        .and_then(|trace| http::HeaderValue::from_str(&trace).ok())
                    {
                        response
                            .headers_mut()
                            .insert("x-debugbundle-trace-id", value);
                    }
                }
                let lifecycle = this.lifecycle.take();
                Poll::Ready(Ok(response.map(|body| InstrumentedBody {
                    inner: Box::pin(body),
                    lifecycle,
                })))
            }
        }
    }
}
pub struct InstrumentedBody<B> {
    inner: Pin<Box<B>>,
    lifecycle: Option<RequestLifecycle>,
}
impl<B: Body> Body for InstrumentedBody<B> {
    type Data = B::Data;
    type Error = B::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let result = if let Some(scope) = &this.lifecycle {
            scope
                .owner
                .handle()
                .run(|| this.inner.as_mut().poll_frame(cx))
        } else {
            this.inner.as_mut().poll_frame(cx)
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
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
