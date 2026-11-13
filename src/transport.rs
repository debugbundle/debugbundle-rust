//! SDK-owned HTTP transport. Credentials live only on authenticated requests.
use serde_json::Value;
use std::time::{Duration, Instant, SystemTime};
#[derive(Clone)]
pub struct HttpTransport {
    client: reqwest::Client,
    endpoint: url::Url,
    token: String,
    timeout: Duration,
}
pub struct Reply {
    pub status: u16,
    pub body: Value,
    pub etag: Option<String>,
    pub retry_at: Option<Instant>,
}
#[derive(Debug)]
pub struct TransportError;
impl HttpTransport {
    pub fn new(
        endpoint: &str,
        token: &str,
        timeout: Duration,
        allow_http: bool,
        proxy: Option<&str>,
    ) -> Option<Self> {
        if endpoint.len() > 8192 {
            return None;
        }
        let endpoint = url::Url::parse(endpoint).ok()?;
        let privacy = crate::privacy::Privacy::new(&[])?;
        if !crate::config::safe_endpoint(&endpoint, allow_http, &privacy)
            || token.is_empty()
            || token.len() > 4096
            || token.bytes().any(|b| b <= 32 || b == 127)
            || timeout.is_zero()
            || timeout > Duration::from_secs(30)
        {
            return None;
        }
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .connect_timeout(timeout)
            .pool_max_idle_per_host(2)
            .user_agent(concat!("debugbundle-rust/", env!("CARGO_PKG_VERSION")));
        if let Some(proxy) = proxy {
            if proxy.len() > 8192 {
                return None;
            }
            builder = builder.proxy(reqwest::Proxy::all(proxy).ok()?);
        }
        let client = builder.build().ok()?;
        Some(Self {
            client,
            endpoint,
            token: token.into(),
            timeout,
        })
    }
    pub async fn send(&self, body: Vec<u8>) -> Result<Reply, TransportError> {
        if body.len() > crate::wire::MAX_BODY_BYTES {
            return Err(TransportError);
        }
        let request = self
            .client
            .post(self.endpoint.clone())
            .header("Content-Type", "application/json")
            .body(body);
        self.perform(request, false).await
    }
    pub async fn config(
        &self,
        etag: Option<&str>,
        service: &str,
        environment: &str,
    ) -> Result<Reply, TransportError> {
        let privacy = crate::privacy::Privacy::new(&[]).ok_or(TransportError)?;
        if !crate::config::technical_identity(service, &privacy)
            || !crate::config::technical_identity(environment, &privacy)
        {
            return Err(TransportError);
        }
        let mut endpoint = self.endpoint.clone();
        let prefix = endpoint.path().strip_suffix("/v1/events").unwrap_or("");
        endpoint.set_path(&format!("{prefix}/v1/sdk/config"));
        endpoint.set_query(None);
        endpoint
            .query_pairs_mut()
            .append_pair("service", service)
            .append_pair("environment", environment);
        let mut request = self.client.get(endpoint);
        if let Some(etag) = etag.filter(|s| s.len() <= 256 && !s.contains(['\r', '\n'])) {
            request = request.header("If-None-Match", etag);
        }
        self.perform(request, true).await
    }
    async fn perform(
        &self,
        request: reqwest::RequestBuilder,
        allow_not_modified: bool,
    ) -> Result<Reply, TransportError> {
        let operation = async {
            let mut response = request
                .bearer_auth(&self.token)
                .header("X-DebugBundle-Evidence", "1")
                .header("Accept-Encoding", "identity")
                .send()
                .await
                .map_err(|_| TransportError)?;
            let received = Instant::now();
            let wall_received = SystemTime::now();
            let status = response.status().as_u16();
            let encoding = response
                .headers()
                .get("Content-Encoding")
                .and_then(|s| s.to_str().ok());
            if encoding.is_some_and(|s| !s.eq_ignore_ascii_case("identity")) {
                return Err(TransportError);
            }
            if response
                .content_length()
                .is_some_and(|n| n > crate::wire::MAX_BODY_BYTES as u64)
            {
                return Err(TransportError);
            }
            let retry_at = response
                .headers()
                .get("Retry-After")
                .and_then(|s| s.to_str().ok())
                .and_then(|s| crate::protocol::retry_after(s, wall_received))
                .and_then(|duration| received.checked_add(duration));
            let etag = response
                .headers()
                .get("ETag")
                .and_then(|s| s.to_str().ok())
                .filter(|s| s.len() <= 256)
                .map(str::to_owned);
            if status == 304 && allow_not_modified {
                return Ok(Reply {
                    status,
                    body: Value::Null,
                    etag,
                    retry_at,
                });
            }
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| TransportError)? {
                if chunk.len() > crate::wire::MAX_BODY_BYTES.saturating_sub(body.len()) {
                    return Err(TransportError);
                }
                body.extend_from_slice(&chunk);
            }
            let text = std::str::from_utf8(&body).map_err(|_| TransportError)?;
            let body = serde_json::from_str(text).map_err(|_| TransportError)?;
            Ok(Reply {
                status,
                body,
                etag,
                retry_at,
            })
        };
        tokio::time::timeout(self.timeout, operation)
            .await
            .map_err(|_| TransportError)?
    }
}
