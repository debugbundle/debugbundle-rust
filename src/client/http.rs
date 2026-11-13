use super::Client;
use crate::{
    http_context::{HttpOptions, RequestData, RequestLifecycle},
    Context, OperationKind, OperationOptions, RequestSnapshot,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU16, Ordering},
    Arc,
};

impl Client {
    pub(crate) fn bind_http_trigger(
        &self,
        operation: &crate::OperationContext,
        header: Option<&str>,
        query: Option<&str>,
    ) {
        // Read only this request's bounded token. A present invalid header must
        // not activate the query fallback, and neither source enters context.
        let query_token = if header.is_none() {
            query
                .filter(|query| query.len() <= 16384)
                .and_then(|query| {
                    url::form_urlencoded::parse(query.as_bytes())
                        .take(256)
                        .find(|(key, _)| key == "_debug_probe")
                        .map(|(_, value)| value)
                })
        } else {
            None
        };
        self.bind_probe_trigger(operation, header, query_token.as_deref());
    }
    #[cfg(any(feature = "axum", feature = "actix-web"))]
    pub(crate) fn http_response_status(&self, context: &Context, status: u16) {
        if self.same_http_client(context) {
            if let Some(request) = &context.http {
                request.status.store(status, Ordering::Release);
            }
        }
    }
    pub(crate) fn same_http_client(&self, context: &Context) -> bool {
        self.inner.as_ref().is_some_and(|inner| {
            context.http.is_some()
                && context
                    .operation
                    .as_ref()
                    .is_some_and(|op| op.matches(&inner.operations))
        })
    }
    pub(crate) fn begin_http(
        &self,
        mut snapshot: RequestSnapshot,
        trace: Option<&str>,
        request_id: Option<&str>,
        framework: &'static str,
        options: &HttpOptions,
    ) -> Option<RequestLifecycle> {
        let inner = self.inner.as_ref()?;
        if inner.stopped.load(Ordering::Acquire) || crate::runtime::internal() {
            return None;
        }
        if snapshot.path.len() > 16384 {
            snapshot.path = "/[withheld]".into();
        }
        let path = snapshot.path.split(['?', '#']).next().unwrap_or("/");
        // Relay and SDK-owned endpoints are transport, not application incidents.
        let ingest = url::Url::parse(&inner.config.endpoint).ok();
        if path == "/debugbundle/browser"
            || path == "/v1/sdk/config"
            || ingest.as_ref().is_some_and(|url| url.path() == path)
            || options
                .excluded_paths
                .iter()
                .any(|excluded| excluded == path)
        {
            return None;
        }
        // Contention must not turn a native Context extractor into a rejection.
        // An unavailable snapshot uses the conservative initial policy.
        let (client_errors, all) = inner
            .state
            .try_lock()
            .ok()
            .map(|state| {
                (
                    std::array::from_fn(|index| {
                        state
                            .policy
                            .immediate(400 + index as u16, path, &snapshot.method)
                    }),
                    state.policy.requests == "all",
                )
            })
            .unwrap_or(([false; 100], false));
        snapshot.method = if snapshot.method.len() <= 16
            && snapshot.method.bytes().all(|b| b.is_ascii_uppercase())
        {
            snapshot.method
        } else {
            "UNKNOWN".into()
        };
        let absolute = format!("https://request.invalid{}", snapshot.path);
        snapshot.path = inner
            .privacy
            .text(&absolute)
            .strip_prefix("https://request.invalid")
            .unwrap_or("/[withheld]")
            .to_owned();
        snapshot.route_template = snapshot
            .route_template
            .filter(|route| route.len() <= 2048)
            .map(|route| inner.privacy.text(&route));
        snapshot.headers = inner
            .privacy
            .protect(&snapshot.headers)
            .filter(Value::is_object)
            .filter(|value| crate::wire::canonical_size(value).is_some_and(|bytes| bytes <= 8192))
            .unwrap_or_else(|| json!({}));
        snapshot.body = snapshot
            .body
            .as_ref()
            .and_then(|body| inner.privacy.protect(body))
            .filter(|body| crate::wire::canonical_size(body).is_some_and(|bytes| bytes <= 8192));
        let data = Arc::new(RequestData {
            snapshot,
            framework,
            version: options.framework_version.clone(),
            status: AtomicU16::new(0),
            all,
            client_errors,
        });
        let mut context = Context::default();
        context.http = Some(data.clone());
        if let Some(id) = request_id {
            context = context.with_request_id(id);
        }
        if !context.correlation.contains_key("request_id") {
            if let Some(id) = crate::identity::fresh() {
                context = context.with_request_id(&id);
            }
        }
        let owner = self.start_operation(OperationOptions {
            kind: OperationKind::Request,
            name: data
                .snapshot
                .route_template
                .clone()
                .unwrap_or_else(|| data.snapshot.path.split('?').next().unwrap_or("/").into()),
            trace_id: trace.and_then(crate::operations::trace_id),
            context,
            ..OperationOptions::default()
        });
        owner
            .is_active()
            .then(|| RequestLifecycle::new(self.clone(), owner, data))
    }
}
