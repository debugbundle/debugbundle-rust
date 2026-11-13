//! One bounded disk replay loop per client; file work stays on its existing worker.
use super::{response, sender, Client, Inner};
use crate::{
    config::ProjectMode,
    protocol,
    relay::spool::{self, Disposition, SpoolStatus, Store, Work},
    wire,
};
use serde_json::json;
use std::{
    sync::{atomic::Ordering, Arc, Weak},
    time::{Duration, Instant},
};
pub(super) fn scope(inner: &Inner) -> String {
    spool::scope(&inner.config.endpoint, &inner.config.project_token)
}
pub(super) struct Admission {
    inner: Arc<Inner>,
}
impl Admission {
    pub(super) fn new(inner: Arc<Inner>) -> Self {
        inner.spool_admitting.fetch_add(1, Ordering::AcqRel);
        Self { inner }
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        self.inner.spool_revision.fetch_add(1, Ordering::AcqRel);
        if let Ok(mut status) = self.inner.spool_status.lock() {
            status.ready = false;
        }
        self.inner.spool_admitting.fetch_sub(1, Ordering::AcqRel);
        self.inner.spool_wake.notify_one();
        self.inner.changed.notify_waiters();
    }
}
impl Client {
    pub(crate) fn start_relay_spool(&self) {
        let Some(inner) = self.inner.as_ref().filter(|inner| {
            inner.config.project_mode == ProjectMode::Connected
                && !inner.stopped.load(Ordering::Acquire)
        }) else {
            return;
        };
        if inner
            .spool_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        if !crate::runtime::spawn(run(Arc::downgrade(inner))) {
            inner.spool_started.store(false, Ordering::Release);
        }
    }
    pub(crate) fn relay_spool_status(&self) -> SpoolStatus {
        self.inner
            .as_ref()
            .and_then(|inner| inner.spool_status.lock().ok())
            .map(|status| status.clone())
            .unwrap_or_default()
    }
    pub(crate) async fn flush_relay_spool(&self, timeout: Duration) -> bool {
        if crate::runtime::internal() {
            return false;
        }
        let Some(inner) = self.inner.as_ref() else {
            return false;
        };
        if !inner.spool_started.load(Ordering::Acquire) {
            return inner.config.project_mode == ProjectMode::LocalOnly;
        }
        let deadline = Instant::now() + timeout.min(Duration::from_secs(30));
        inner.spool_wake.notify_one();
        loop {
            let changed = inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if inner.spool_status.lock().ok().is_some_and(|status| {
                status.ready && status.pending_records == 0 && status.storage_failures == 0
            }) && inner.spool_admitting.load(Ordering::Acquire) == 0
                && !inner.spool_lease.pending.load(Ordering::Acquire)
            {
                return true;
            }
            if inner.stopped.load(Ordering::Acquire) || Instant::now() >= deadline {
                return false;
            }
            if tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), changed)
                .await
                .is_err()
            {
                return false;
            }
        }
    }
}
async fn run(weak: Weak<Inner>) {
    loop {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        if inner.stopped.load(Ordering::Acquire) {
            return;
        }
        let released = inner.spool_lease.released.notified();
        tokio::pin!(released);
        released.as_mut().enable();
        if inner.spool_lease.pending.load(Ordering::Acquire) {
            let stopped = inner.stop.notified();
            tokio::pin!(stopped);
            stopped.as_mut().enable();
            if inner.stopped.load(Ordering::Acquire) {
                return;
            }
            tokio::select! {_=released=>continue,_=stopped=>return};
        }
        let revision = inner.spool_revision.load(Ordering::Acquire);
        let directory = inner.config.spool_dir.clone();
        let bound = scope(&inner);
        let control = spool::LeaseControl {
            reservation: inner.spool_lease.clone(),
            slot: inner.slot.clone(),
            cleanup: inner.callbacks.native_cleanup(),
        };
        let scanned = tokio::time::timeout(
            inner.config.request_timeout,
            inner.callbacks.call(move || {
                Store::open(&directory)?.scan(
                    &bound,
                    chrono::Utc::now().timestamp_millis(),
                    control,
                )
            }),
        )
        .await;
        let mut delay = Duration::from_secs(1);
        match scanned {
            Ok(Ok(Ok(scan))) => {
                if let Ok(mut status) = inner.spool_status.lock() {
                    if inner.spool_revision.load(Ordering::Acquire) == revision
                        && inner.spool_admitting.load(Ordering::Acquire) == 0
                    {
                        *status = scan.status;
                    } else {
                        status.ready = false;
                    }
                }
                inner.changed.notify_waiters();
                if let Some(work) = scan.work {
                    let completed = forward(&inner, work).await;
                    if completed {
                        delay = Duration::ZERO;
                    }
                }
            }
            _ => {
                if let Ok(mut status) = inner.spool_status.lock() {
                    status.ready = false;
                    status.storage_failures = status.storage_failures.saturating_add(1);
                }
                inner.changed.notify_waiters();
            }
        }
        let stopped = inner.stop.notified();
        tokio::pin!(stopped);
        stopped.as_mut().enable();
        if inner.stopped.load(Ordering::Acquire) {
            return;
        }
        tokio::select! {_=stopped=>return,_=inner.spool_wake.notified()=>{},_=tokio::time::sleep(delay)=>{}}
    }
}
async fn forward(inner: &Arc<Inner>, mut work: Work) -> bool {
    let completed = forward_body(inner, &mut work).await;
    drop(work.lease);
    completed
}
async fn persist(inner: &Arc<Inner>, work: &mut Work) -> bool {
    let directory = inner.config.spool_dir.clone();
    let record = work.record.clone();
    let previous = work.metadata.clone();
    match tokio::time::timeout(
        inner.config.request_timeout,
        inner
            .callbacks
            .call(move || Store::open(&directory)?.update(&record, &previous)),
    )
    .await
    {
        Ok(Ok(Ok(bytes))) => {
            work.metadata = bytes;
            true
        }
        _ => {
            if let Ok(mut status) = inner.spool_status.lock() {
                status.ready = false;
                status.storage_failures = status.storage_failures.saturating_add(1);
            }
            inner.changed.notify_waiters();
            false
        }
    }
}
async fn forward_body(inner: &Arc<Inner>, work: &mut Work) -> bool {
    let deadline = Instant::now() + inner.config.request_timeout;
    loop {
        if inner.stopped.load(Ordering::Acquire) || Instant::now() >= deadline {
            return false;
        }
        if work
            .record
            .states
            .iter()
            .all(|state| *state == Disposition::Accepted)
        {
            work.record.completed_at = Some(
                chrono::Utc::now()
                    .timestamp_millis()
                    .max(work.record.created_at),
            );
            return persist(inner, work).await;
        }
        let features = inner
            .state
            .lock()
            .ok()
            .map(|state| state.features.clone())
            .unwrap_or_default();
        let mut indices = Vec::new();
        let mut events = Vec::new();
        let mut size = 13usize;
        let mut locally_changed = false;
        for (index, event) in work.events.iter().enumerate() {
            if work.record.states[index] != Disposition::Pending {
                continue;
            }
            let Some(event) =
                crate::relay::validation::for_delivery(event, &inner.privacy, &features)
            else {
                work.record.states[index] = Disposition::Terminal;
                locally_changed = true;
                continue;
            };
            let Some(bytes) = serde_json::to_vec(&event).ok().map(|body| body.len()) else {
                return false;
            };
            if bytes > wire::MAX_EVENT_BYTES {
                work.record.states[index] = Disposition::Terminal;
                locally_changed = true;
                continue;
            }
            let extra = bytes + usize::from(!events.is_empty());
            if extra > wire::MAX_BODY_BYTES.saturating_sub(size) {
                break;
            }
            size += extra;
            indices.push(index);
            events.push(event);
        }
        if locally_changed && !persist(inner, work).await {
            return false;
        }
        if events.is_empty() {
            return false;
        }
        let digests = events
            .iter()
            .map(|event| {
                serde_json::to_vec(event)
                    .ok()
                    .map(|bytes| spool::digest(&bytes))
            })
            .collect::<Option<Vec<_>>>();
        let Some(digests) = digests else {
            return false;
        };
        let Ok(body) = serde_json::to_vec(&json!({"events":events})) else {
            return false;
        };
        let sent = tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            sender::send(inner, body, events, deadline),
        )
        .await;
        let mut retry_after = None;
        let mut complete_chunk = false;
        if let Ok(Some((reply, native, _))) = sent {
            let ack = if (200..300).contains(&reply.status) {
                reply
                    .body
                    .as_ref()
                    .and_then(|body| protocol::acknowledgement(body, indices.len()))
            } else {
                None
            };
            response::observe(inner, &reply, native, ack.is_some());
            retry_after = reply.retry_after;
            if let Some(ack) = ack {
                complete_chunk = ack.accepted.len() == indices.len();
                for accepted in ack.accepted {
                    let index = indices[accepted];
                    work.record.states[index] = Disposition::Accepted;
                    work.record.acknowledged_digests[index] = Some(digests[accepted].clone());
                }
                for terminal in ack.terminal {
                    work.record.states[indices[terminal]] = Disposition::Terminal;
                }
            }
        }
        if !complete_chunk {
            work.record.failures = work.record.failures.saturating_add(1).min(64);
            let delay = retry_after
                .unwrap_or(Duration::from_millis(
                    100u64.saturating_mul(1u64 << work.record.failures.min(12)),
                ))
                .min(Duration::from_secs(300));
            work.record.next_attempt_at = chrono::Utc::now()
                .timestamp_millis()
                .saturating_add(delay.as_millis().min(i64::MAX as u128) as i64);
        }
        if !persist(inner, work).await {
            return false;
        }
        if !complete_chunk {
            return false;
        }
    }
}
