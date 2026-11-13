use super::{RelayOptions, RelayRequest};
use std::{
    collections::HashMap,
    net::IpAddr,
    time::{Duration, Instant},
};
#[cfg(test)]
mod tests;
fn origin_url(value: &str, referer: bool) -> Option<String> {
    if value.len() > 8192 {
        return None;
    }
    let url = url::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || (!referer && (url.path() != "/" || url.query().is_some() || url.fragment().is_some()))
    {
        return None;
    }
    Some(url.origin().ascii_serialization())
}
fn trusted(request: &RelayRequest, options: &RelayOptions) -> bool {
    request
        .peer
        .is_some_and(|peer| options.trusted_proxies.contains(&peer))
}
pub(super) fn origin(request: &RelayRequest, options: &RelayOptions) -> Option<String> {
    let origin = match &request.origin {
        Some(origin) => origin_url(origin, false)?,
        None => origin_url(request.referer.as_deref()?, true)?,
    };
    if !options.allowed_origins.is_empty() {
        return options.allowed_origins.contains(&origin).then_some(origin);
    }
    let (host, scheme) = if trusted(request, options) {
        (
            request.forwarded_host.as_deref().unwrap_or(&request.host),
            request
                .forwarded_proto
                .as_deref()
                .unwrap_or(&request.scheme),
        )
    } else {
        (request.host.as_str(), request.scheme.as_str())
    };
    if host.len() > 2048
        || host.contains(['/', '?', '#', '@', ',', '\\'])
        || !matches!(scheme, "http" | "https")
    {
        return None;
    }
    (origin_url(&format!("{scheme}://{host}"), false)? == origin).then_some(origin)
}
pub(super) fn peer(request: &RelayRequest, options: &RelayOptions) -> Option<IpAddr> {
    if trusted(request, options) {
        if let Some(peer) = request
            .forwarded_for
            .as_deref()
            .filter(|peer| peer.len() <= 45)
            .and_then(|peer| peer.parse().ok())
        {
            return Some(peer);
        }
    }
    request.peer
}
#[derive(Default)]
pub(super) struct Limiter {
    entries: HashMap<Option<IpAddr>, (Instant, u32)>,
}
impl Limiter {
    pub(super) fn allow(&mut self, peer: Option<IpAddr>, limit: u32, now: Instant) -> bool {
        self.entries.retain(|_, (start, _)| {
            now.saturating_duration_since(*start) < Duration::from_secs(60)
        });
        if !self.entries.contains_key(&peer) && self.entries.len() >= 4096 {
            return false;
        }
        let entry = self.entries.entry(peer).or_insert((now, 0));
        if entry.1 >= limit {
            return false;
        }
        entry.1 += 1;
        true
    }
}
