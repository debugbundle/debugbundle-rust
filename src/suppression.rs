//! Bounded local storm suppression, independent of persistent fingerprints.
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};
#[derive(Default)]
pub(crate) struct Suppression {
    buckets: HashMap<String, Bucket>,
}
struct Bucket {
    window: Instant,
    last: Instant,
    emitted: Instant,
    recent: VecDeque<Instant>,
    normal: u8,
    suppressed: u64,
    looping: bool,
    first_seen: String,
    last_seen: String,
}
pub(crate) struct Aggregate {
    pub fingerprint: String,
    pub count: u64,
    pub first_seen: String,
    pub last_seen: String,
}
impl Suppression {
    pub fn observe(&mut self, key: &str, now: Instant, occurred: &str) -> bool {
        if key.len() > 128 || occurred.len() > 64 {
            return false;
        }
        if !self.buckets.contains_key(key) && self.buckets.len() >= 1000 {
            if let Some(oldest) = self
                .buckets
                .iter()
                .min_by_key(|(_, b)| b.last)
                .map(|(key, _)| key.clone())
            {
                self.buckets.remove(&oldest);
            }
        }
        let bucket = self
            .buckets
            .entry(key.to_owned())
            .or_insert_with(|| Bucket {
                window: now,
                last: now,
                emitted: now,
                recent: VecDeque::new(),
                normal: 0,
                suppressed: 0,
                looping: false,
                first_seen: occurred.into(),
                last_seen: occurred.into(),
            });
        if now.saturating_duration_since(bucket.last) >= Duration::from_secs(60) {
            *bucket = Bucket {
                window: now,
                last: now,
                emitted: now,
                recent: VecDeque::new(),
                normal: 0,
                suppressed: 0,
                looping: false,
                first_seen: occurred.into(),
                last_seen: occurred.into(),
            };
        } else if !bucket.looping
            && now.saturating_duration_since(bucket.window) >= Duration::from_secs(30)
        {
            bucket.window = now;
            bucket.normal = 0;
        }
        bucket.last = now;
        bucket.last_seen = occurred.into();
        while bucket
            .recent
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) > Duration::from_secs(2))
        {
            bucket.recent.pop_front();
        }
        if bucket.recent.len() == 11 {
            bucket.recent.pop_front();
        }
        bucket.recent.push_back(now);
        if bucket.recent.len() > 10 {
            bucket.looping = true;
        }
        if !bucket.looping && bucket.normal < 3 {
            bucket.normal += 1;
            true
        } else {
            bucket.suppressed = bucket.suppressed.saturating_add(1);
            false
        }
    }
    pub fn collect(&mut self, now: Instant, limit: usize) -> Vec<Aggregate> {
        let mut output = Vec::new();
        for (key, bucket) in &mut self.buckets {
            if output.len() >= limit.min(16) {
                break;
            }
            if bucket.suppressed > 0
                && now.saturating_duration_since(bucket.emitted) >= Duration::from_secs(30)
            {
                output.push(Aggregate {
                    fingerprint: key.clone(),
                    count: bucket.suppressed,
                    first_seen: bucket.first_seen.clone(),
                    last_seen: bucket.last_seen.clone(),
                });
                bucket.suppressed = 0;
                bucket.emitted = now;
                bucket.first_seen = bucket.last_seen.clone();
            }
        }
        output
    }
}
#[cfg(test)]
mod tests;
