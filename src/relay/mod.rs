//! Browser relay ingress. Connected durable delivery is withheld until persisted.
#[cfg(any(feature = "axum", feature = "actix-web"))]
pub(crate) mod native;
mod security;
pub(crate) mod spool;
pub use spool::SpoolStatus;
pub(crate) mod validation;
use crate::{privacy::Privacy, Client};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    net::IpAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};

/// A store owns its minute-window clock and must update each key atomically.
/// The SDK calls it on the existing bounded callback worker, with a finite wait.
pub trait RateLimitStore: Send + Sync + 'static {
    fn allow(&self, key: RateLimitKey) -> Result<bool, RateLimitError>;
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateLimitKey {
    /// Digest bound to endpoint and project credentials; never the token itself.
    pub namespace: String,
    pub peer: Option<IpAddr>,
    pub limit_per_minute: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateLimitError {
    Unavailable,
}

#[derive(Clone, Debug)]
pub struct RelayOptions {
    pub allowed_origins: Vec<String>,
    pub trusted_proxies: Vec<IpAddr>,
    pub rate_limit_per_minute: u32,
    pub durable_write: bool,
    pub service: Option<String>,
    pub environment: Option<String>,
}
impl Default for RelayOptions {
    fn default() -> Self {
        Self {
            allowed_origins: Vec::new(),
            trusted_proxies: Vec::new(),
            rate_limit_per_minute: 60,
            durable_write: true,
            service: None,
            environment: None,
        }
    }
}
#[derive(Default)]
pub struct RelayRequest {
    pub method: String,
    pub scheme: String,
    pub host: String,
    pub origin: Option<String>,
    pub referer: Option<String>,
    pub content_type: Option<String>,
    pub peer: Option<IpAddr>,
    pub forwarded_host: Option<String>,
    pub forwarded_proto: Option<String>,
    pub forwarded_for: Option<String>,
    pub evidence: bool,
}
pub struct RelayResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Value>,
}
impl RelayResponse {
    fn status(status: u16) -> Self {
        Self {
            status,
            headers: BTreeMap::new(),
            body: None,
        }
    }
    fn malformed() -> Self {
        let mut response = Self::status(400);
        response.body = Some(
            json!({"accepted":0,"rejected":0,"errors":[{"index":-1,"reason":"malformed_payload"}]}),
        );
        response
    }
    fn cors(mut self, origin: &str) -> Self {
        self.headers
            .insert("access-control-allow-origin".into(), origin.into());
        self.headers.insert("vary".into(), "Origin".into());
        self
    }
}
struct Inner {
    client: Client,
    privacy: Privacy,
    options: RelayOptions,
    budget: Arc<IngressBudget>,
}
/// All native handlers for one client share ingress and IP accounting. Framework
/// worker factories must not multiply retained uploads or reset rate windows.
#[derive(Default)]
pub(crate) struct IngressBudget {
    limiter: Mutex<security::Limiter>,
    active: AtomicUsize,
    idle: tokio::sync::Notify,
}
struct Admission(Arc<Inner>);
impl Drop for Admission {
    fn drop(&mut self) {
        self.0.budget.active.fetch_sub(1, Ordering::AcqRel);
        self.0.budget.idle.notify_waiters();
    }
}
impl Inner {
    fn admit(self: &Arc<Self>) -> Option<Admission> {
        for _ in 0..32 {
            let active = self.budget.active.load(Ordering::Acquire);
            if active >= 8 {
                return None;
            }
            if self
                .budget
                .active
                .compare_exchange_weak(active, active + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(Admission(self.clone()));
            }
        }
        None
    }
}
#[derive(Clone)]
pub struct Relay {
    inner: Option<Arc<Inner>>,
}
impl Relay {
    pub fn new(client: Client, mut options: RelayOptions) -> Self {
        let Some(privacy) = client.relay_privacy() else {
            return Self { inner: None };
        };
        let Some(budget) = client.relay_budget() else {
            return Self { inner: None };
        };
        if options.allowed_origins.len() > 64
            || options.trusted_proxies.len() > 16
            || options.rate_limit_per_minute == 0
            || options.rate_limit_per_minute > 10000
            || [&options.service, &options.environment]
                .iter()
                .any(|value| {
                    value
                        .as_ref()
                        .is_some_and(|value| !crate::config::technical_identity(value, &privacy))
                })
        {
            return Self { inner: None };
        }
        let Some(origins) = options
            .allowed_origins
            .iter()
            .map(|origin| crate::config::trace_origin(origin))
            .collect::<Option<Vec<_>>>()
        else {
            return Self { inner: None };
        };
        options.allowed_origins = origins;
        if options.durable_write {
            client.start_relay_spool();
        }
        Self {
            inner: Some(Arc::new(Inner {
                client,
                privacy,
                options,
                budget,
            })),
        }
    }
    pub fn status(&self) -> SpoolStatus {
        self.inner
            .as_ref()
            .map(|inner| inner.client.relay_spool_status())
            .unwrap_or_default()
    }
    pub async fn flush_async(&self, timeout: std::time::Duration) -> bool {
        if crate::runtime::internal() {
            return false;
        }
        let deadline = std::time::Instant::now() + timeout.min(std::time::Duration::from_secs(30));
        match &self.inner {
            Some(inner) => {
                loop {
                    let idle = inner.budget.idle.notified();
                    tokio::pin!(idle);
                    idle.as_mut().enable();
                    if inner.budget.active.load(Ordering::Acquire) == 0 {
                        break;
                    }
                    if tokio::time::timeout(
                        deadline.saturating_duration_since(std::time::Instant::now()),
                        idle,
                    )
                    .await
                    .is_err()
                    {
                        return false;
                    }
                }
                inner
                    .client
                    .flush_relay_spool(
                        deadline.saturating_duration_since(std::time::Instant::now()),
                    )
                    .await
            }
            None => false,
        }
    }
    /// Bounded request-side persistence is an explicit relay operation, not capture.
    pub async fn handle(&self, request: &RelayRequest, body: &[u8]) -> RelayResponse {
        match self.begin(request, Some(body.len() as u64)).await {
            Ok(admission) => admission.finish(body).await,
            Err(response) => response,
        }
    }
    pub(crate) async fn begin<'a>(
        &self,
        request: &'a RelayRequest,
        declared: Option<u64>,
    ) -> Result<RequestAdmission<'a>, RelayResponse> {
        if crate::runtime::internal() {
            return Err(RelayResponse::status(503));
        }
        let Some(inner) = &self.inner else {
            return Err(RelayResponse::status(503));
        };
        if !inner.client.relay_live() {
            return Err(RelayResponse::status(503));
        };
        let Some(origin) = security::origin(request, &inner.options) else {
            return Err(RelayResponse::status(403));
        };
        if request.method == "OPTIONS" {
            let mut response = RelayResponse::status(204);
            response.headers.insert(
                "access-control-allow-methods".into(),
                "POST, OPTIONS".into(),
            );
            response.headers.insert(
                "access-control-allow-headers".into(),
                "content-type,x-debugbundle-evidence".into(),
            );
            return Err(response.cors(&origin));
        }
        if request.method != "POST" {
            return Err(RelayResponse::status(405).cors(&origin));
        }
        if declared.is_some_and(|length| length > crate::wire::MAX_BODY_BYTES as u64) {
            return Err(RelayResponse::status(413).cors(&origin));
        }
        if !request.content_type.as_deref().is_some_and(|value| {
            value.len() <= 128
                && value
                    .split(';')
                    .next()
                    .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
        }) {
            return Err(RelayResponse::malformed().cors(&origin));
        }
        let peer = security::peer(request, &inner.options);
        if !inner
            .budget
            .limiter
            .try_lock()
            .ok()
            .is_some_and(|mut limiter| {
                limiter.allow(peer, inner.options.rate_limit_per_minute, Instant::now())
            })
        {
            let mut response = RelayResponse::status(429);
            response.headers.insert("retry-after".into(), "60".into());
            return Err(response.cors(&origin));
        }
        let Some(permit) = inner.admit() else {
            return Err(RelayResponse::status(503).cors(&origin));
        };
        let admission = RequestAdmission {
            inner: inner.clone(),
            request,
            origin,
            _permit: permit,
        };
        match inner
            .client
            .relay_rate_limit(peer, inner.options.rate_limit_per_minute)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                let mut response = admission.failure(429);
                response.headers.insert("retry-after".into(), "60".into());
                return Err(response);
            }
            Err(()) => return Err(admission.failure(503)),
        }
        Ok(admission)
    }
    async fn process(inner: &Inner, request: &RelayRequest, body: &[u8]) -> RelayResponse {
        let deadline = Instant::now() + inner.client.relay_timeout();
        if body.len() > crate::wire::MAX_BODY_BYTES {
            return RelayResponse::status(413);
        }
        let Ok(input) = serde_json::from_slice::<Value>(body) else {
            return RelayResponse::malformed();
        };
        let Some(root) = input.as_object().filter(|root| root.len() == 1) else {
            return RelayResponse::malformed();
        };
        let Some(batch) = root
            .get("batch")
            .and_then(Value::as_array)
            .filter(|batch| batch.len() <= 100)
        else {
            return RelayResponse::malformed();
        };
        let features = inner.client.relay_features();
        let mut events = Vec::new();
        let mut errors = Vec::new();
        let mut legacy = Vec::new();
        for (index, candidate) in batch.iter().enumerate() {
            if let Some(event) =
                validation::event(candidate, &inner.privacy, &inner.options, &features)
            {
                match inner.client.relay_finalize(event, deadline).await {
                    Ok(Some(event)) => match validation::for_delivery(
                        &event,
                        &inner.privacy,
                        &inner.client.relay_features(),
                    ) {
                        Some(event) => events.push(event),
                        None => {
                            errors.push(json!({"index":index,"reason":"local_policy"}));
                            legacy.push(format!("batch[{index}]: Event withheld by local policy."));
                        }
                    },
                    Ok(None) => {
                        errors.push(json!({"index":index,"reason":"local_policy"}));
                        legacy.push(format!("batch[{index}]: Event withheld by local policy."));
                    }
                    Err(()) => return RelayResponse::status(503),
                }
            } else {
                errors.push(json!({"index":index,"reason":"invalid_event"}));
                legacy.push(if candidate.get("event_type").and_then(Value::as_str)==Some("backend_exception") {
                    format!("batch[{index}]: Unsupported browser relay event type backend_exception.")
                } else {
                    format!("batch[{index}]: invalid browser relay event payload.")
                });
            }
        }
        let accepted = events.len();
        if !events.is_empty()
            && !inner
                .client
                .relay_deliver(events, inner.options.durable_write)
                .await
        {
            return RelayResponse::status(503);
        }
        // Existing browsers opt into indexed outcomes through evidence_errors;
        // older relay clients retain the established mixed-batch 400 behavior.
        let status = if errors.is_empty() || request.evidence {
            202
        } else {
            400
        };
        let mut response = RelayResponse::status(status);
        let mut body = json!({"accepted":accepted,"rejected":errors.len(),"errors":legacy});
        if request.evidence {
            body["evidence_errors"] = json!(errors);
            body["evidence_capabilities"] = crate::wire::descriptor(&inner.client.relay_features());
        }
        response.body = Some(body);
        response
    }
}

pub(crate) struct RequestAdmission<'a> {
    inner: Arc<Inner>,
    request: &'a RelayRequest,
    origin: String,
    _permit: Admission,
}
impl RequestAdmission<'_> {
    fn failure(self, status: u16) -> RelayResponse {
        RelayResponse::status(status).cors(&self.origin)
    }
    pub(crate) async fn finish(self, body: &[u8]) -> RelayResponse {
        Relay::process(&self.inner, self.request, body)
            .await
            .cors(&self.origin)
    }
    #[cfg(any(feature = "axum", feature = "actix-web"))]
    pub(crate) fn reject(self, status: u16) -> RelayResponse {
        self.failure(status)
    }
}

/// Verified transport facts for an Axum TLS listener or custom connection adapter.
/// Populate this from the accepted socket, never from request headers.
#[derive(Clone, Copy, Debug, Default)]
pub struct RelayConnection {
    pub secure: bool,
    pub peer: Option<IpAddr>,
}
