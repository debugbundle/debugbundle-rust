//! Revocable operation owners and poll-scoped context for Send and local futures.
use crate::{model::text, Context};
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, Weak,
    },
    task::{Context as TaskContext, Poll},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default)]
pub enum OperationKind {
    Request,
    Command,
    Job,
    #[default]
    Unknown,
}
impl OperationKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Command => "command",
            Self::Job => "job",
            Self::Unknown => "unknown",
        }
    }
}
#[derive(Default)]
pub struct OperationOptions {
    pub kind: OperationKind,
    pub name: String,
    pub limit: Option<Duration>,
    pub parent: Option<OperationContext>,
    pub trace_id: Option<String>,
    pub context: Context,
}

#[derive(Debug)]
pub(crate) struct OperationBudget {
    pub enabled: AtomicBool,
    count: AtomicUsize,
    bytes: AtomicUsize,
}
impl Default for OperationBudget {
    fn default() -> Self {
        Self {
            enabled: AtomicBool::new(true),
            count: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
        }
    }
}
impl OperationBudget {
    #[cfg(feature = "testkit")]
    pub(crate) fn resources(&self) -> (usize, usize) {
        (
            self.count.load(Ordering::Acquire),
            self.bytes.load(Ordering::Acquire),
        )
    }
    fn reserve(&self, bytes: usize) -> bool {
        fn add(counter: &AtomicUsize, amount: usize, max: usize) -> bool {
            for _ in 0..32 {
                let old = counter.load(Ordering::Acquire);
                let Some(next) = old.checked_add(amount).filter(|n| *n <= max) else {
                    return false;
                };
                if counter
                    .compare_exchange_weak(old, next, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    return true;
                }
            }
            false
        }
        if !add(&self.count, 1, 1024) {
            return false;
        }
        if !add(&self.bytes, bytes, 262144) {
            self.count.fetch_sub(1, Ordering::AcqRel);
            return false;
        }
        true
    }
    fn release(&self, bytes: usize) {
        self.count.fetch_sub(1, Ordering::AcqRel);
        self.bytes.fetch_sub(bytes, Ordering::AcqRel);
    }
}
#[derive(Debug)]
struct State {
    id: String,
    kind: OperationKind,
    name: Option<String>,
    limit: Option<Duration>,
    started: Instant,
    active: AtomicBool,
    parent: Option<Weak<State>>,
    budget: Arc<OperationBudget>,
    depth: usize,
    context: Context,
    trigger: Mutex<Option<crate::probes::TriggerBinding>>,
    bytes: usize,
}
#[derive(Clone, Debug, Default)]
pub struct OperationContext {
    state: Option<Arc<State>>,
}
#[derive(Default)]
pub struct Operation {
    handle: OperationContext,
}
impl Drop for Operation {
    fn drop(&mut self) {
        self.stop();
    }
}
impl Operation {
    pub fn is_active(&self) -> bool {
        self.handle.is_active()
    }
    pub fn handle(&self) -> OperationContext {
        self.handle.clone()
    }
    pub fn context(&self) -> Context {
        self.handle.context()
    }
    pub fn stop(&mut self) {
        self.handle.finish();
    }
    pub fn scope<F: Future>(self, future: F) -> ScopedFuture<F> {
        let context = self.context();
        ScopedFuture {
            future: Box::pin(future),
            context,
            owner: Some(self),
        }
    }
    pub fn run<T>(self, work: impl FnOnce() -> T) -> T {
        let _context = enter(self.context());
        work()
    }
}
impl OperationContext {
    pub(crate) fn finish(&self) {
        if let Some(state) = &self.state {
            if state.active.swap(false, Ordering::AcqRel) {
                state.budget.release(state.bytes);
            }
        }
    }
    pub fn run<T>(&self, work: impl FnOnce() -> T) -> T {
        let _context = enter(self.context());
        work()
    }
    pub fn is_active(&self) -> bool {
        let Some(state) = &self.state else {
            return false;
        };
        if !state.budget.enabled.load(Ordering::Acquire) {
            return false;
        }
        let mut current = Some(state.clone());
        for _ in 0..32 {
            let Some(state) = current else {
                return true;
            };
            if !state.active.load(Ordering::Acquire) {
                return false;
            }
            current = match &state.parent {
                Some(parent) => {
                    let Some(parent) = parent.upgrade() else {
                        return false;
                    };
                    Some(parent)
                }
                None => None,
            };
        }
        current.is_none()
    }
    pub(crate) fn belongs(&self, budget: &Arc<OperationBudget>) -> bool {
        self.state
            .as_ref()
            .is_some_and(|state| Arc::ptr_eq(&state.budget, budget))
    }
    pub(crate) fn matches(&self, budget: &Arc<OperationBudget>) -> bool {
        self.is_active() && self.belongs(budget)
    }
    pub(crate) fn bind_trigger(
        &self,
        budget: &Arc<OperationBudget>,
        binding: Option<crate::probes::TriggerBinding>,
    ) -> bool {
        if !self.matches(budget) {
            return false;
        }
        let Some(state) = &self.state else {
            return false;
        };
        if !matches!(state.kind, OperationKind::Request) {
            return false;
        }
        let Ok(mut trigger) = state.trigger.try_lock() else {
            return false;
        };
        *trigger = binding;
        true
    }
    pub(crate) fn trigger(
        &self,
        budget: &Arc<OperationBudget>,
    ) -> Option<crate::probes::TriggerBinding> {
        if !self.matches(budget) {
            return None;
        }
        let mut current = self.state.clone();
        for _ in 0..32 {
            let state = current?;
            if matches!(state.kind, OperationKind::Request) {
                return state.trigger.try_lock().ok()?.clone();
            }
            current = state.parent.as_ref().and_then(Weak::upgrade);
        }
        None
    }
    pub fn operation_id(&self) -> Option<&str> {
        self.state.as_ref().map(|state| state.id.as_str())
    }
    pub fn context(&self) -> Context {
        let Some(state) = &self.state else {
            return Context::default();
        };
        let mut context = state.context.clone();
        context.operation = Some(self.clone());
        context
    }
    pub fn wrap<F: Future>(&self, future: F) -> ScopedFuture<F> {
        ScopedFuture {
            future: Box::pin(future),
            context: self.context(),
            owner: None,
        }
    }
    pub(crate) fn execution(&self, budget: &Arc<OperationBudget>) -> Option<Value> {
        if !self.matches(budget) {
            return None;
        }
        let state = self.state.as_ref()?;
        Some(
            json!({"version":1,"kind":state.kind.as_str(),"name":state.name,"elapsed_ms":state.started.elapsed().as_secs_f64()*1000.0,"limit_ms":state.limit.map(|limit|limit.as_secs_f64()*1000.0)}),
        )
    }
}

pub(crate) fn start(
    budget: Arc<OperationBudget>,
    privacy: &crate::privacy::Privacy,
    mut options: OperationOptions,
) -> Operation {
    if !budget.enabled.load(Ordering::Acquire)
        || budget.count.load(Ordering::Acquire) >= 1024
        || budget.bytes.load(Ordering::Acquire) >= 262144
    {
        return Operation::default();
    }
    let ambient = current();
    if ambient
        .as_ref()
        .and_then(|context| context.operation.as_ref())
        .is_some_and(|op| op.belongs(&budget) && !op.is_active())
    {
        return Operation::default();
    }
    let current =
        ambient.filter(|ctx| ctx.operation.as_ref().is_some_and(|op| op.matches(&budget)));
    let parent = options
        .parent
        .take()
        .or_else(|| current.as_ref().and_then(|ctx| ctx.operation.clone()));
    if parent
        .as_ref()
        .is_some_and(|parent| !parent.matches(&budget))
    {
        return Operation::default();
    }
    let depth = parent
        .as_ref()
        .and_then(|parent| parent.state.as_ref())
        .map_or(1, |state| state.depth + 1);
    if depth > 32 {
        return Operation::default();
    }
    let Some(id) = crate::identity::fresh() else {
        return Operation::default();
    };
    let mut context = if options.context.is_empty() {
        current
            .or_else(|| parent.as_ref().map(OperationContext::context))
            .unwrap_or_default()
    } else {
        options.context
    };
    context = context.protect_with(privacy);
    context.operation = None;
    let trace = options
        .trace_id
        .as_deref()
        .and_then(trace_id)
        .or_else(|| {
            context
                .correlation
                .get("trace_id")
                .and_then(Value::as_str)
                .and_then(trace_id)
        })
        .or_else(crate::identity::fresh);
    let Some(trace) = trace else {
        return Operation::default();
    };
    context = context.with_trace_id(&trace);
    let name = if options.name.len() > 16384 {
        Some("[REDACTED]".into())
    } else {
        (!options.name.trim().is_empty()).then(|| text(&privacy.text(&options.name), 128))
    };
    let bytes = context.retained_bytes()
        + id.len()
        + name.as_ref().map_or(0, String::len)
        + 64
        + if matches!(options.kind, OperationKind::Request) {
            1024
        } else {
            0
        };
    if !budget.reserve(bytes) {
        return Operation::default();
    }
    let state = Arc::new(State {
        id,
        kind: options.kind,
        name,
        limit: options.limit,
        started: Instant::now(),
        active: AtomicBool::new(true),
        parent: parent.and_then(|parent| parent.state.map(|state| Arc::downgrade(&state))),
        budget,
        depth,
        context,
        trigger: Mutex::new(None),
        bytes,
    });
    Operation {
        handle: OperationContext { state: Some(state) },
    }
}
pub(crate) fn trace_id(value: &str) -> Option<String> {
    if value.len() != 36 {
        return None;
    }
    let uuid = uuid::Uuid::parse_str(value).ok()?;
    (uuid.get_version_num() == 4 && uuid.get_variant() == uuid::Variant::RFC4122)
        .then(|| value.to_owned())
}
thread_local! {static CURRENT:RefCell<Option<Context>>=const{RefCell::new(None)};}
pub(crate) fn current() -> Option<Context> {
    CURRENT.with(|current| current.try_borrow().ok().and_then(|value| value.clone()))
}
struct Entered {
    previous: Option<Context>,
    installed: bool,
}
fn enter(context: Context) -> Entered {
    CURRENT.with(|slot| {
        if let Ok(mut value) = slot.try_borrow_mut() {
            Entered {
                previous: value.replace(context),
                installed: true,
            }
        } else {
            Entered {
                previous: None,
                installed: false,
            }
        }
    })
}
impl Drop for Entered {
    fn drop(&mut self) {
        if self.installed {
            CURRENT.with(|slot| {
                if let Ok(mut current) = slot.try_borrow_mut() {
                    *current = self.previous.take();
                }
            });
        }
    }
}

pub struct ScopedFuture<F: Future> {
    future: Pin<Box<F>>,
    context: Context,
    owner: Option<Operation>,
}
impl<F: Future> Future for ScopedFuture<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let guard = enter(this.context.clone());
        let result = this.future.as_mut().poll(cx);
        drop(guard);
        if result.is_ready() {
            this.owner.take();
        }
        result
    }
}
