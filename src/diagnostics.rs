//! One joint allowance for association metadata and every pending diagnostic.
//! Counts are consumed on admission; ACK/drop frees bytes, never allowances.
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashMap},
    time::{Duration, Instant},
};
const MAX_BYTES: usize = 65536;
const WINDOW: Duration = Duration::from_secs(300);
#[derive(Clone)]
struct Window {
    started: Instant,
    at: DateTime<Utc>,
    failure_at: Option<DateTime<Utc>>,
    failure_id: Option<String>,
    attempts: BTreeSet<String>,
    observations: usize,
    pending: usize,
    ambiguous: bool,
    resource: bool,
}
impl Window {
    fn new(now: Instant, wall: DateTime<Utc>) -> Self {
        Self {
            started: now,
            at: wall,
            failure_at: None,
            failure_id: None,
            attempts: BTreeSet::new(),
            observations: 0,
            pending: 0,
            ambiguous: false,
            resource: false,
        }
    }
    fn cost(&self, key: &str) -> usize {
        key.len()+64+crate::wire::canonical_size(&json!({"at":self.at,"failure_at":self.failure_at,"failure_id":self.failure_id,"attempts":self.attempts,"observations":self.observations,"pending":self.pending,"ambiguous":self.ambiguous,"resource":self.resource})).unwrap_or(MAX_BYTES)
    }
}
struct Receipt {
    key: String,
    kind: String,
    attempt: String,
    bytes: usize,
    cost: usize,
    occurred: DateTime<Utc>,
}
#[derive(Default)]
pub(crate) struct Ledger {
    windows: HashMap<String, Window>,
    pending: HashMap<String, Receipt>,
    held: usize,
}
fn key(payload: &Value, event: &Value) -> Option<String> {
    serde_json::to_string(&json!([
        payload["failure_ref"],
        payload["scope"],
        event["correlation"]["trace_id"]
    ]))
    .ok()
}
fn occurred(event: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(event["occurred_at"].as_str()?)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}
impl Ledger {
    #[cfg(feature = "testkit")]
    pub(crate) fn resources(&self) -> (usize, usize, usize) {
        (self.bytes(), self.windows.len(), self.pending.len())
    }
    fn bytes(&self) -> usize {
        self.held
            + self
                .windows
                .iter()
                .map(|(key, window)| window.cost(key))
                .sum::<usize>()
    }
    fn prune(&mut self, now: Instant) {
        self.windows.retain(|_, window| {
            window.pending > 0 || now.saturating_duration_since(window.started) < WINDOW
        });
    }
    pub fn register(&mut self, event: &Value, now: Instant, wall: DateTime<Utc>) {
        if !["backend_exception", "frontend_exception", "log_event"]
            .iter()
            .any(|kind| event["event_type"] == *kind)
        {
            return;
        }
        let reference = &event["context"]["evidence_reference"];
        if !crate::wire::valid_reference(reference) {
            return;
        }
        let Some(key) = key(reference, event) else {
            return;
        };
        let Some(id) = event["event_id"].as_str() else {
            return;
        };
        let Some(at) = occurred(event) else {
            return;
        };
        self.prune(now);
        let existing = self.windows.get(&key);
        if existing.is_none() && self.windows.len() >= 50 {
            return;
        }
        let mut window = existing.cloned().unwrap_or_else(|| Window::new(now, wall));
        if window
            .failure_id
            .as_deref()
            .is_some_and(|previous| previous != id)
        {
            window.ambiguous = true;
        } else {
            window.failure_id = Some(id.into());
            window.failure_at.get_or_insert(at);
        }
        let old = existing.map_or(0, |window| window.cost(&key));
        if self.bytes().saturating_sub(old) + window.cost(&key) <= MAX_BYTES {
            self.windows.insert(key, window);
        }
    }
    pub fn reserve(
        &mut self,
        event: &Value,
        bytes: usize,
        now: Instant,
        wall: DateTime<Utc>,
    ) -> bool {
        let Some(id) = event["event_id"].as_str() else {
            return false;
        };
        if bytes > MAX_BYTES || self.pending.contains_key(id) {
            return false;
        }
        let payload = &event["payload"];
        let Some(key) = key(payload, event) else {
            return false;
        };
        let Some(at) = occurred(event) else {
            return false;
        };
        if at > wall + chrono::Duration::seconds(60) || at < wall - chrono::Duration::minutes(5) {
            return false;
        }
        self.prune(now);
        let existing = self.windows.get(&key);
        if existing.is_none() && self.windows.len() >= 50 {
            return false;
        }
        let mut window = existing.cloned().unwrap_or_else(|| Window::new(now, wall));
        if window.ambiguous
            || now.saturating_duration_since(window.started) >= WINDOW
            || window.observations >= 20
        {
            return false;
        }
        if window
            .failure_at
            .is_some_and(|failure| at < failure || at > failure + chrono::Duration::minutes(5))
        {
            return false;
        }
        let kind = payload["kind"].as_str().unwrap_or("");
        let attempt = payload["attempt_id"].as_str().unwrap_or("");
        if kind == "recovery" {
            if !window.attempts.contains(attempt) && window.attempts.len() >= 5 {
                return false;
            }
            window.attempts.insert(attempt.into());
        } else if kind == "resource_observation" {
            if window.resource
                || window.failure_id.as_deref() != payload["source_event_id"].as_str()
                || window
                    .failure_at
                    .is_none_or(|failure| at > failure + chrono::Duration::seconds(30))
            {
                return false;
            }
            window.resource = true;
        } else {
            return false;
        }
        let old = existing.map_or(0, |window| window.cost(&key));
        window.observations += 1;
        window.pending += 1;
        let cost = bytes + id.len() + key.len() + 64;
        if self.bytes().saturating_sub(old) + window.cost(&key) + cost > MAX_BYTES {
            return false;
        }
        self.windows.insert(key.clone(), window);
        self.held += cost;
        self.pending.insert(
            id.into(),
            Receipt {
                key,
                kind: kind.into(),
                attempt: attempt.into(),
                bytes,
                cost,
                occurred: at,
            },
        );
        true
    }
    pub fn eligible(&self, id: &str, event: &Value, now: Instant) -> bool {
        let Some(receipt) = self.pending.get(id) else {
            return false;
        };
        let Some(window) = self.windows.get(&receipt.key) else {
            return false;
        };
        key(&event["payload"], event).is_some_and(|key| key == receipt.key)
            && !window.ambiguous
            && now.saturating_duration_since(window.started) < WINDOW
            && occurred(event).is_some_and(|at| {
                window.failure_at.is_none_or(|failure| {
                    at >= failure && at <= failure + chrono::Duration::minutes(5)
                })
            })
    }
    pub fn replace(
        &mut self,
        previous: Option<&str>,
        event: &Value,
        bytes: usize,
        now: Instant,
        wall: DateTime<Utc>,
    ) -> bool {
        if occurred(event).is_none_or(|at| {
            at > wall + chrono::Duration::seconds(60) || at < wall - chrono::Duration::minutes(5)
        }) {
            if let Some(previous) = previous {
                self.release(previous);
            }
            return false;
        }
        let Some(id) = event["event_id"].as_str() else {
            if let Some(previous) = previous {
                self.release(previous);
            }
            return false;
        };
        let payload = &event["payload"];
        let next = key(payload, event);
        if let Some((previous, receipt)) = previous.and_then(|previous| {
            self.pending
                .get(previous)
                .map(|receipt| (previous, receipt))
        }) {
            if next.as_deref() == Some(&receipt.key)
                && payload["kind"].as_str() == Some(&receipt.kind)
                && payload["attempt_id"].as_str().unwrap_or("") == receipt.attempt
            {
                let old = receipt.cost;
                let cost = bytes + id.len() + receipt.key.len() + 64;
                if (id != previous && self.pending.contains_key(id))
                    || self.bytes().saturating_sub(old) + cost > MAX_BYTES
                    || !self.eligible(previous, event, now)
                {
                    self.release(previous);
                    return false;
                }
                let Some(mut receipt) = self.pending.remove(previous) else {
                    return false;
                };
                receipt.bytes = bytes;
                receipt.cost = cost;
                receipt.occurred = occurred(event).unwrap_or(wall);
                self.held = self.held.saturating_sub(old) + cost;
                self.pending.insert(id.into(), receipt);
                return true;
            }
        }
        if let Some(previous) = previous {
            self.release(previous);
        }
        self.reserve(event, bytes, now, wall)
    }
    pub fn release(&mut self, id: &str) {
        if let Some(receipt) = self.pending.remove(id) {
            self.held = self.held.saturating_sub(receipt.cost);
            if let Some(window) = self.windows.get_mut(&receipt.key) {
                window.pending = window.pending.saturating_sub(1);
            }
        }
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests;
