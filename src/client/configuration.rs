//! Authenticated immutable-client configuration, independent of callback work.
use super::{Client, Inner};
use crate::{policy::Policy, probes::Remote, wire};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
    time::Duration,
};

struct RefreshGuard<'a>(&'a AtomicBool);
impl Drop for RefreshGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
impl Client {
    /// Explicit administrative refresh. Concurrent checks coalesce by declining
    /// a second request; application capture never performs this network call.
    pub async fn refresh_remote_config_async(&self, timeout: Duration) -> bool {
        let Some(inner) = self
            .inner
            .as_ref()
            .filter(|inner| inner.http.is_some() && !inner.stopped.load(Ordering::Acquire))
        else {
            return false;
        };
        let weak = Arc::downgrade(inner);
        let (tx, rx) = tokio::sync::oneshot::channel();
        if !crate::runtime::spawn(async move {
            let result = if let Some(inner) = weak.upgrade() {
                request(&inner).await
            } else {
                false
            };
            let _ = tx.send(result);
        }) {
            return false;
        }
        tokio::time::timeout(timeout.min(Duration::from_secs(30)), rx)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(false)
    }
}
pub(super) async fn run(weak: Weak<Inner>) {
    let mut failures = 0u32;
    loop {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        if inner.stopped.load(Ordering::Acquire) {
            return;
        }
        if request(&inner).await {
            failures = 0;
        } else {
            failures = failures.saturating_add(1);
        }
        let (initialized, paid, interval) = {
            let Ok(state) = inner.state.lock() else {
                return;
            };
            (
                state.remote.initialized,
                state.remote.remote_enabled,
                state.remote.interval,
            )
        };
        // Unknown credentials receive only three bounded discovery attempts.
        // A known Free/disabled response ends the automatic task immediately.
        if (initialized && !paid) || (!initialized && failures >= 3) {
            return;
        }
        let delay = if initialized {
            interval
        } else {
            Duration::from_secs(1u64 << failures.min(2))
        };
        let stopped = inner.stop.notified();
        tokio::pin!(stopped);
        stopped.as_mut().enable();
        if inner.stopped.load(Ordering::Acquire) {
            return;
        }
        tokio::select! {_ = stopped=>return,_ = tokio::time::sleep(delay)=>{}}
    }
}
async fn request(inner: &Arc<Inner>) -> bool {
    if inner
        .config_inflight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }
    let _guard = RefreshGuard(&inner.config_inflight);
    let Some(http) = inner.http.as_ref() else {
        return false;
    };
    let etag = inner
        .state
        .lock()
        .ok()
        .and_then(|state| state.config_etag.clone());
    let stopped = inner.stop.notified();
    tokio::pin!(stopped);
    stopped.as_mut().enable();
    if inner.stopped.load(Ordering::Acquire) {
        return false;
    }
    let reply = tokio::select! {_ = stopped=>return false,reply=http.config(etag.as_deref(),&inner.config.service,&inner.config.environment)=>reply};
    let Ok(reply) = reply else {
        return false;
    };
    if reply.status == 304 {
        return inner
            .state
            .lock()
            .ok()
            .is_some_and(|state| state.remote.initialized);
    }
    if !(200..300).contains(&reply.status) || !wire::preflight(&reply.body) {
        return false;
    }
    let (Some(policy), Some(mut remote)) = (
        Policy::parse(&reply.body),
        Remote::parse(&reply.body, inner.config.probes_poll_interval),
    ) else {
        return false;
    };
    remote
        .directives
        .retain(|directive| directive.safe(&inner.privacy));
    if inner.stopped.load(Ordering::Acquire) {
        return false;
    }
    let Ok(mut state) = inner.state.lock() else {
        return false;
    };
    inner.log_level.store(
        policy
            .logs
            .map_or(u8::MAX, |level| level.max(inner.config.log_level) as u8),
        Ordering::Release,
    );
    state.policy = policy;
    state.features = wire::capabilities(&reply.body["evidence_capabilities"]);
    state.remote = remote;
    state.config_etag = reply.etag;
    if !state.remote.enabled {
        state.probes.clear();
    }
    true
}
