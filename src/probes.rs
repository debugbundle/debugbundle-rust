//! Bounded always-on rings and authenticated remote directive snapshots.
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    time::Duration,
};

#[derive(Clone, Copy, Default)]
pub struct ProbeOptions {
    pub heavy: bool,
}
struct Entry {
    item: Value,
    bytes: usize,
}
#[derive(Default)]
pub(crate) struct Rings {
    labels: BTreeMap<String, VecDeque<Entry>>,
    bytes: usize,
}
impl Rings {
    pub fn can_label(&self, label: &str, max: usize, reserved: Option<&str>) -> bool {
        self.labels.contains_key(label)
            || self.labels.len()
                + usize::from(reserved.is_some_and(|s| s != label && !self.labels.contains_key(s)))
                < max
    }
    pub fn insert(&mut self, label: &str, data: Value, max_entries: usize) -> bool {
        let item = json!({"label":label,"data":data,"timestamp":crate::client::now(),"activation_id":null});
        let Some(bytes) = serde_json::to_vec(&item).ok().map(|b| b.len()) else {
            return false;
        };
        let previous = self
            .labels
            .get(label)
            .filter(|entries| entries.len() >= max_entries)
            .and_then(|entries| entries.front())
            .map_or(0, |entry| entry.bytes);
        if bytes > 262144usize.saturating_sub(self.bytes.saturating_sub(previous)) {
            return false;
        }
        let entries = self.labels.entry(label.into()).or_default();
        if entries.len() >= max_entries {
            if let Some(old) = entries.pop_front() {
                self.bytes = self.bytes.saturating_sub(old.bytes);
            }
        }
        self.bytes += bytes;
        entries.push_back(Entry { item, bytes });
        true
    }
    pub fn snapshot(&self) -> Option<Value> {
        let items = self
            .labels
            .values()
            .flat_map(|entries| entries.iter().map(|entry| entry.item.clone()))
            .collect::<Vec<_>>();
        (!items.is_empty()).then(|| json!({"version":1,"items":items}))
    }
    pub fn clear(&mut self) {
        self.labels.clear();
        self.bytes = 0;
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Directive {
    pub id: String,
    pub pattern: String,
    pub service: String,
    pub environment: String,
    pub expires: DateTime<Utc>,
}
impl Directive {
    pub fn safe(&self, privacy: &crate::privacy::Privacy) -> bool {
        [&self.pattern, &self.service, &self.environment]
            .iter()
            .all(|value| !value.chars().any(char::is_control) && privacy.text(value) == **value)
    }
    pub fn matches(&self, label: &str, service: &str, environment: &str) -> bool {
        self.expires > Utc::now()
            && (self.service == "*" || self.service == service)
            && (self.environment == "*" || self.environment == environment)
            && (self.pattern == "*"
                || self.pattern == label
                || self.pattern.strip_suffix(".*").is_some_and(|prefix| {
                    label == prefix
                        || label
                            .strip_prefix(prefix)
                            .is_some_and(|tail| tail.starts_with('.'))
                }))
    }
    pub fn parse(value: &Value, expiry: &str, max_ttl: i64) -> Option<Self> {
        let id = value
            .get("activation_id")
            .or_else(|| value.get("id"))?
            .as_str()?;
        if value
            .get("activation_id")
            .zip(value.get("id"))
            .is_some_and(|(a, b)| a != b)
            || uuid::Uuid::parse_str(id).is_err()
        {
            return None;
        }
        let pattern = value["label_pattern"].as_str()?;
        let service = value["service"].as_str()?;
        let environment = value["environment"].as_str()?;
        if pattern.is_empty()
            || pattern.len() > 256
            || service.is_empty()
            || service.len() > 128
            || environment.is_empty()
            || environment.len() > 128
        {
            return None;
        }
        if pattern.contains('*')
            && pattern != "*"
            && !(pattern.ends_with(".*") && pattern.matches('*').count() == 1)
        {
            return None;
        }
        let expires = DateTime::parse_from_rfc3339(value[expiry].as_str()?)
            .ok()?
            .with_timezone(&Utc);
        let remaining = expires.signed_duration_since(Utc::now());
        if remaining <= chrono::Duration::zero() || remaining > chrono::Duration::seconds(max_ttl) {
            return None;
        }
        Some(Self {
            id: id.into(),
            pattern: pattern.into(),
            service: service.into(),
            environment: environment.into(),
            expires,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TriggerBinding {
    pub directive: Directive,
    pub key_hash: [u8; 32],
}
pub(crate) fn key_hash(key: &str) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(key.as_bytes()).into()
}
pub(crate) fn verify(token: &str, key: &str) -> Option<TriggerBinding> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use hmac::{Hmac, Mac};
    if token.len() > 8192 || key.is_empty() || key.len() > 512 {
        return None;
    }
    let (payload, signature) = token.strip_prefix("dbundle_probe_")?.split_once('.')?;
    if payload.len() > 5462 || signature.len() != 43 {
        return None;
    }
    let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
    if signature.len() != 32 {
        return None;
    }
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key.as_bytes()).ok()?;
    mac.update(payload.as_bytes());
    mac.verify_slice(&signature).ok()?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    if bytes.len() > 4096 {
        return None;
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let map = value.as_object()?;
    if map.len() != 5
        || !map.keys().all(|key| {
            [
                "activation_id",
                "label_pattern",
                "service",
                "environment",
                "trigger_expires_at",
            ]
            .contains(&key.as_str())
        })
    {
        return None;
    }
    Some(TriggerBinding {
        directive: Directive::parse(&value, "trigger_expires_at", 86400)?,
        key_hash: key_hash(key),
    })
}

pub(crate) struct Remote {
    pub initialized: bool,
    pub enabled: bool,
    pub remote_enabled: bool,
    pub directives: Vec<Directive>,
    pub key: String,
    pub interval: Duration,
}
impl Default for Remote {
    fn default() -> Self {
        Self {
            initialized: false,
            enabled: true,
            remote_enabled: false,
            directives: Vec::new(),
            key: String::new(),
            interval: Duration::from_secs(60),
        }
    }
}
impl Remote {
    #[cfg(any(feature = "http", test))]
    pub fn parse(body: &Value, default_interval: Duration) -> Option<Self> {
        let enabled = body
            .get("probes_enabled")
            .map(Value::as_bool)
            .unwrap_or(Some(true))?;
        let remote = body
            .get("remote_probes_enabled")
            .map(Value::as_bool)
            .unwrap_or(Some(false))?;
        let mut directives = Vec::new();
        if let Some(values) = body.get("active_probes") {
            let values = values.as_array()?;
            if values.len() > 100 {
                return None;
            }
            for value in values {
                if let Some(directive) = Directive::parse(value, "expires_at", 3600) {
                    directives.push(directive);
                }
            }
        }
        let key = body
            .get("trigger_token_key")
            .and_then(Value::as_str)
            .filter(|s| s.len() <= 512)
            .unwrap_or("");
        let interval = if directives
            .iter()
            .any(|directive| directive.expires > Utc::now())
        {
            Duration::from_secs(15)
        } else {
            body.get("poll_interval_ms")
                .and_then(crate::wire::unsigned)
                .map(Duration::from_millis)
                .filter(|d| *d >= Duration::from_secs(15) && *d <= Duration::from_secs(300))
                .unwrap_or(default_interval)
        };
        Some(Self {
            initialized: true,
            enabled,
            remote_enabled: enabled && remote,
            directives: if enabled && remote {
                directives
            } else {
                Vec::new()
            },
            key: if enabled && remote {
                key.into()
            } else {
                String::new()
            },
            interval,
        })
    }
    pub fn matching(&self, label: &str, service: &str, environment: &str) -> Vec<Directive> {
        if !self.enabled || !self.remote_enabled {
            return Vec::new();
        }
        self.directives
            .iter()
            .filter(|directive| directive.matches(label, service, environment))
            .cloned()
            .collect()
    }
    pub fn piggyback(&mut self, body: &Value, privacy: &crate::privacy::Privacy) {
        if !self.remote_enabled {
            return;
        }
        self.directives = body["probe_directives"]["active_probes"]
            .as_array()
            .filter(|values| values.len() <= 100)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| Directive::parse(value, "expires_at", 3600))
                    .filter(|directive| directive.safe(privacy))
                    .collect()
            })
            .unwrap_or_default();
        self.interval = if self.directives.is_empty() {
            Duration::from_secs(60)
        } else {
            Duration::from_secs(15)
        };
    }
}

#[cfg(test)]
mod tests;
