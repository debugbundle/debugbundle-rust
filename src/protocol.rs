//! Strict ingestion acknowledgement and retry semantics.
use serde_json::Value;
use std::time::{Duration, SystemTime};
#[derive(Debug, PartialEq)]
pub struct Acknowledgement {
    pub accepted: Vec<usize>,
    pub retry: Vec<usize>,
    pub terminal: Vec<usize>,
}
pub fn acknowledgement(body: &Value, count: usize) -> Option<Acknowledgement> {
    if count > 100 {
        return None;
    }
    let accepted = usize::try_from(crate::wire::unsigned(&body["accepted"])?).ok()?;
    let rejected = usize::try_from(crate::wire::unsigned(&body["rejected"])?).ok()?;
    let errors = body["errors"].as_array()?;
    if accepted.checked_add(rejected)? != count || errors.len() != rejected {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    let mut result = Acknowledgement {
        accepted: Vec::new(),
        retry: Vec::new(),
        terminal: Vec::new(),
    };
    for error in errors {
        let index = usize::try_from(crate::wire::unsigned(&error["index"])?).ok()?;
        let reason = error["reason"].as_str()?;
        if index >= count || !seen.insert(index) || reason.len() > 256 || reason.trim().is_empty() {
            return None;
        }
        if [
            "rate_limited",
            "monthly_quota_exceeded",
            "analytics_quota_exceeded",
        ]
        .contains(&reason)
        {
            result.retry.push(index);
        } else {
            result.terminal.push(index);
        }
    }
    result.accepted = (0..count).filter(|index| !seen.contains(index)).collect();
    Some(result)
}
pub fn retry_after(hint: &str, received: SystemTime) -> Option<Duration> {
    let value = hint.trim();
    if value.is_empty() {
        return None;
    }
    if value.bytes().all(|b| b.is_ascii_digit()) {
        // Saturate while parsing: hostile numeric hints never overflow first.
        let seconds = value.bytes().fold(0u64, |n, b| {
            n.saturating_mul(10)
                .saturating_add(u64::from(b - b'0'))
                .min(300)
        });
        return Some(Duration::from_secs(seconds));
    }
    let date = httpdate::parse_http_date(value).ok()?;
    Some(
        date.duration_since(received)
            .unwrap_or(Duration::ZERO)
            .min(Duration::from_secs(300)),
    )
}
