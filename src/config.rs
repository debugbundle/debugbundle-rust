//! Configuration is explicit and server capture policy cannot be overridden here.
use crate::{delivery::Transport, Level};
use serde_json::Value;
use std::{path::PathBuf, sync::Arc, time::Duration};

pub type BeforeSend = Arc<dyn Fn(Value) -> Option<Value> + Send + Sync + 'static>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectMode {
    Connected,
    LocalOnly,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    MissingToken,
    InvalidEndpoint,
    InvalidLimit,
    InvalidPrivacy,
    MissingTransport,
}

#[derive(Clone)]
pub struct Config {
    pub project_token: String,
    pub endpoint: String,
    pub service: String,
    pub environment: String,
    pub enabled: bool,
    pub project_mode: ProjectMode,
    pub local_events_dir: PathBuf,
    pub spool_dir: PathBuf,
    pub sample_rate: f64,
    pub log_level: Level,
    pub batch_size: usize,
    pub flush_interval: Duration,
    pub max_buffered_events: usize,
    pub max_buffered_bytes: usize,
    pub max_event_age: Duration,
    pub request_timeout: Duration,
    pub redact_fields: Vec<String>,
    pub before_send: Option<BeforeSend>,
    #[cfg(feature = "local")]
    pub relay_rate_limit_store: Option<Arc<dyn crate::relay::RateLimitStore>>,
    pub transport: Option<Arc<dyn Transport>>,
    pub allow_insecure_http: bool,
    pub proxy: Option<String>,
    pub capture_exception_evidence: bool,
    pub capture_recovery_observations: bool,
    pub evidence_capabilities: Option<Value>,
    pub max_probe_labels: usize,
    pub max_probe_entries_per_label: usize,
    pub probe_flush_on_error: bool,
    pub probes_poll_interval: Duration,
    pub trace_propagation_targets: Vec<String>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            project_token: String::new(),
            endpoint: "https://api.debugbundle.com/v1/events".into(),
            service: "rust-service".into(),
            environment: "development".into(),
            enabled: true,
            project_mode: ProjectMode::Connected,
            local_events_dir: ".debugbundle/local/events".into(),
            spool_dir: ".debugbundle/local/browser-relay-spool".into(),
            sample_rate: 1.0,
            log_level: Level::Warning,
            batch_size: 25,
            flush_interval: Duration::from_secs(5),
            max_buffered_events: 1000,
            max_buffered_bytes: 8 * 1024 * 1024,
            max_event_age: Duration::from_secs(600),
            request_timeout: Duration::from_secs(5),
            redact_fields: Vec::new(),
            before_send: None,
            #[cfg(feature = "local")]
            relay_rate_limit_store: None,
            transport: None,
            allow_insecure_http: false,
            proxy: None,
            capture_exception_evidence: true,
            capture_recovery_observations: false,
            evidence_capabilities: None,
            max_probe_labels: 50,
            max_probe_entries_per_label: 10,
            probe_flush_on_error: true,
            probes_poll_interval: Duration::from_secs(60),
            trace_propagation_targets: Vec::new(),
        }
    }
}
impl Config {
    pub fn new(project_token: impl Into<String>, service: impl Into<String>) -> Self {
        Self {
            project_token: project_token.into(),
            service: service.into(),
            ..Self::default()
        }
    }
    pub fn local(service: impl Into<String>, directory: impl Into<PathBuf>) -> Self {
        Self {
            service: service.into(),
            project_mode: ProjectMode::LocalOnly,
            local_events_dir: directory.into(),
            ..Self::default()
        }
    }
    /// Reads only named SDK configuration variables; never enumerates the host.
    pub fn from_env() -> Self {
        let mut config = Self::default();
        for (key, target) in [
            ("DEBUGBUNDLE_PROJECT_TOKEN", &mut config.project_token),
            ("DEBUGBUNDLE_ENDPOINT", &mut config.endpoint),
            ("DEBUGBUNDLE_SERVICE", &mut config.service),
            ("DEBUGBUNDLE_ENVIRONMENT", &mut config.environment),
        ] {
            if let Ok(value) = std::env::var(key) {
                *target = value;
            }
        }
        config
    }
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.enabled {
            return Ok(());
        }
        if self.service.len() > 128
            || self.service.trim().is_empty()
            || self.environment.len() > 128
            || self.environment.trim().is_empty()
        {
            return Err(ConfigError::InvalidLimit);
        }
        if self.project_mode == ProjectMode::LocalOnly && self.transport.is_some() {
            return Err(ConfigError::InvalidLimit);
        }
        if self.project_mode == ProjectMode::Connected
            && (self.project_token.is_empty()
                || self.project_token.len() > 4096
                || self.project_token.bytes().any(|b| b <= 32 || b == 127))
        {
            return Err(ConfigError::MissingToken);
        }
        let privacy =
            crate::privacy::Privacy::new(&self.redact_fields).ok_or(ConfigError::InvalidPrivacy)?;
        if !technical_identity(&self.service, &privacy)
            || !technical_identity(&self.environment, &privacy)
        {
            return Err(ConfigError::InvalidPrivacy);
        }
        if self.endpoint.len() > 8192 {
            return Err(ConfigError::InvalidEndpoint);
        }
        if self.trace_propagation_targets.len() > 64
            || self
                .trace_propagation_targets
                .iter()
                .any(|target| trace_origin(target).is_none())
        {
            return Err(ConfigError::InvalidEndpoint);
        }
        let endpoint = url::Url::parse(&self.endpoint).map_err(|_| ConfigError::InvalidEndpoint)?;
        if !safe_endpoint(&endpoint, self.allow_insecure_http, &privacy) {
            return Err(ConfigError::InvalidEndpoint);
        }
        if !self.sample_rate.is_finite()
            || !(0.0..=1.0).contains(&self.sample_rate)
            || !(1..=100).contains(&self.batch_size)
            || !(1..=1000).contains(&self.max_buffered_events)
            || !(1024..=8 * 1024 * 1024).contains(&self.max_buffered_bytes)
            || self.flush_interval.is_zero()
            || self.flush_interval > Duration::from_secs(300)
            || self.max_event_age.is_zero()
            || self.max_event_age > Duration::from_secs(600)
            || self.request_timeout.is_zero()
            || self.request_timeout > Duration::from_secs(30)
            || !(1..=50).contains(&self.max_probe_labels)
            || !(1..=10).contains(&self.max_probe_entries_per_label)
            || self.probes_poll_interval < Duration::from_secs(15)
            || self.probes_poll_interval > Duration::from_secs(300)
        {
            return Err(ConfigError::InvalidLimit);
        }
        if self
            .evidence_capabilities
            .as_ref()
            .is_some_and(|v| !crate::wire::valid_descriptor(v))
        {
            return Err(ConfigError::InvalidLimit);
        }
        #[cfg(not(feature = "http"))]
        if self.project_mode == ProjectMode::Connected && self.transport.is_none() {
            return Err(ConfigError::MissingTransport);
        }
        #[cfg(not(feature = "local"))]
        if self.project_mode == ProjectMode::LocalOnly {
            return Err(ConfigError::MissingTransport);
        }
        Ok(())
    }
}
pub(crate) fn technical_identity(value: &str, privacy: &crate::privacy::Privacy) -> bool {
    value.len() <= 128
        && !value.trim().is_empty()
        && !value.chars().any(char::is_control)
        && privacy.text(value) == value
}
pub(crate) fn safe_endpoint(
    url: &url::Url,
    allow_http: bool,
    privacy: &crate::privacy::Privacy,
) -> bool {
    if !(url.scheme() == "https" || (allow_http && url.scheme() == "http"))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let Ok(protected) = url::Url::parse(&privacy.text(url.as_str())) else {
        return false;
    };
    // Compare decoded query pairs so harmless form-encoding normalization does
    // not reject an endpoint; any protected credential value still differs.
    url.scheme() == protected.scheme()
        && url.host() == protected.host()
        && url.port() == protected.port()
        && url.username() == protected.username()
        && url.password() == protected.password()
        && url.path() == protected.path()
        && url.fragment() == protected.fragment()
        && url.query_pairs().eq(protected.query_pairs())
}

pub(crate) fn trace_origin(value: &str) -> Option<String> {
    if value.len() > 2048 {
        return None;
    }
    let url = url::Url::parse(value).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none())
    .then(|| url.origin().ascii_serialization())
}
