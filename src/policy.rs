//! Server-owned policy snapshots; backend capture-rule actions stay server-side.
use crate::{Level, RequestSnapshot};
use serde_json::Value;

#[derive(Clone)]
pub(crate) struct Policy {
    pub preset: String,
    pub logs: Option<Level>,
    pub requests: String,
    pub diagnostics: bool,
    pub probes_standalone: bool,
    pub statuses: Vec<u16>,
    pub paths: Vec<PathRule>,
}
#[derive(Clone)]
pub(crate) struct PathRule {
    status: u16,
    path: String,
    methods: Vec<String>,
}
impl Policy {
    pub fn legacy() -> Self {
        Self {
            preset: "balanced".into(),
            diagnostics: true,
            ..Self::minimal()
        }
    }
    pub fn minimal() -> Self {
        Self {
            preset: "minimal".into(),
            logs: Some(Level::Warning),
            requests: "failures_only".into(),
            diagnostics: false,
            probes_standalone: false,
            statuses: Vec::new(),
            paths: Vec::new(),
        }
    }
    pub fn parse(body: &Value) -> Option<Self> {
        let Some(p) = body.get("capture_policy") else {
            return Some(Self::legacy());
        };
        let preset = p["preset"].as_str()?;
        if !["minimal", "balanced", "investigative"].contains(&preset) {
            return None;
        }
        let logs = match p["capture_logs"].as_str()? {
            "off" => None,
            "error" => Some(Level::Error),
            "warning" => Some(Level::Warning),
            "info" => Some(Level::Info),
            _ => return None,
        };
        let requests = p["capture_request_events"].as_str()?;
        if !["off", "failures_only", "filtered", "all"].contains(&requests) {
            return None;
        }
        let probes = match p["capture_probe_events"].as_str()? {
            "buffer_only" => false,
            "standalone_when_activated" => true,
            _ => return None,
        };
        let diagnostics = if let Some(v) = p.get("capture_diagnostic_context") {
            v.as_bool()?
        } else {
            true
        };
        let mut statuses = Vec::new();
        if let Some(values) = p.get("immediate_client_error_statuses") {
            let values = values.as_array()?;
            if values.len() > 100 {
                return None;
            }
            for value in values {
                let n = u16::try_from(crate::wire::unsigned(value)?).ok()?;
                if !(400..500).contains(&n) {
                    return None;
                }
                if !statuses.contains(&n) {
                    statuses.push(n);
                }
            }
        }
        let mut paths = Vec::new();
        if let Some(values) = p.get("immediate_client_error_path_rules") {
            let values = values.as_array()?;
            if values.len() > 100 {
                return None;
            }
            for value in values {
                let status = u16::try_from(crate::wire::unsigned(&value["status_code"])?).ok()?;
                let path = value["path_pattern"].as_str()?;
                if !(400..500).contains(&status)
                    || !path.starts_with('/')
                    || path.len() > 2048
                    || path.matches('*').count() > 1
                    || (path.contains('*') && !path.ends_with('*'))
                {
                    return None;
                }
                let mut methods = Vec::new();
                if let Some(values) = value.get("methods") {
                    let values = values.as_array()?;
                    if values.len() > 16 {
                        return None;
                    }
                    for method in values {
                        let method = method.as_str()?;
                        if method.is_empty()
                            || method.len() > 16
                            || !method.bytes().all(|b| b.is_ascii_uppercase())
                        {
                            return None;
                        }
                        methods.push(method.into());
                    }
                }
                paths.push(PathRule {
                    status,
                    path: path.into(),
                    methods,
                });
            }
        }
        Some(Self {
            preset: preset.into(),
            logs,
            requests: requests.into(),
            diagnostics: diagnostics && preset != "minimal",
            probes_standalone: probes,
            statuses,
            paths,
        })
    }
    pub fn log(&self, level: Level, local: Level) -> bool {
        level >= local && self.logs.is_some_and(|server| level >= server)
    }
    pub fn immediate(&self, status: u16, path: &str, method: &str) -> bool {
        if status >= 500 {
            return true;
        }
        if self.statuses.contains(&status) {
            return true;
        }
        let path = path.split(['?', '#']).next().unwrap_or("/");
        if self.paths.iter().any(|r| {
            r.status == status
                && (r.methods.is_empty()
                    || r.methods.iter().any(|m| m.eq_ignore_ascii_case(method)))
                && r.path
                    .strip_suffix('*')
                    .map(|p| path.starts_with(p))
                    .unwrap_or(path == r.path)
        }) {
            return true;
        }
        match self.preset.as_str() {
            "balanced" => [408, 423, 424, 425, 429].contains(&status),
            "investigative" => [408, 409, 423, 424, 425, 429].contains(&status),
            _ => false,
        }
    }
    pub fn request(&self, request: &RequestSnapshot) -> bool {
        self.immediate(request.status, &request.path, &request.method) || self.requests == "all"
    }
    pub fn allows(&self, event: &Value, local: Level) -> bool {
        match event["event_type"].as_str() {
            Some("log_event") => event["payload"]["level"]
                .as_str()
                .and_then(Level::parse)
                .is_some_and(|level| self.log(level, local)),
            Some("request_event") => {
                self.immediate(
                    crate::wire::unsigned(&event["payload"]["response_status"]).unwrap_or(0) as u16,
                    event["payload"]["path"].as_str().unwrap_or("/"),
                    event["payload"]["method"].as_str().unwrap_or("UNKNOWN"),
                ) || self.requests == "all"
            }
            Some("diagnostic_context") => self.diagnostics,
            Some("probe_event") => self.probes_standalone,
            _ => true,
        }
    }
}
