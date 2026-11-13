//! Exact SDK tokens and bounded local admission receipts, never text heuristics.
use crate::ExceptionSnapshot;
#[cfg(any(feature = "log", feature = "tracing"))]
use crate::{operations::OperationBudget, Context};
#[cfg(any(feature = "log", feature = "tracing"))]
use std::sync::{Arc, Weak};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
#[derive(Clone, Debug)]
pub struct ExceptionOccurrence {
    id: String,
    snapshot: ExceptionSnapshot,
}
impl ExceptionOccurrence {
    pub fn new(snapshot: &ExceptionSnapshot) -> Option<Self> {
        Some(Self {
            id: crate::identity::fresh()?,
            snapshot: snapshot.clone(),
        })
    }
    pub fn snapshot(&self) -> &ExceptionSnapshot {
        &self.snapshot
    }
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
}
struct Receipt {
    token: String,
    operation: String,
    event: String,
    at: Instant,
    ttl: Duration,
}
#[derive(Default)]
pub(crate) struct Registry {
    receipts: VecDeque<Receipt>,
}
impl Registry {
    pub fn contains(&self, token: &str, operation: &str, now: Instant) -> bool {
        self.receipts.iter().any(|receipt| {
            receipt.token == token
                && receipt.operation == operation
                && now.saturating_duration_since(receipt.at) < receipt.ttl
        })
    }
    #[cfg(any(feature = "log", feature = "tracing", test))]
    pub fn record(
        &mut self,
        token: &str,
        operation: &str,
        event: &str,
        now: Instant,
        ttl: Duration,
    ) {
        self.receipts
            .retain(|receipt| now.saturating_duration_since(receipt.at) < receipt.ttl);
        if self.receipts.len() >= 128 {
            self.receipts.pop_front();
        }
        self.receipts.push_back(Receipt {
            token: token.into(),
            operation: operation.into(),
            event: event.into(),
            at: now,
            ttl: ttl.min(Duration::from_secs(30)),
        });
    }
    pub fn revoke(&mut self, event: &str) {
        self.receipts.retain(|receipt| receipt.event != event);
    }
    pub fn clear(&mut self) {
        self.receipts.clear();
    }
}
#[cfg(any(feature = "log", feature = "tracing"))]
#[derive(Clone)]
pub(crate) struct Association {
    pub budget: Weak<OperationBudget>,
    pub occurrence: ExceptionOccurrence,
    pub context: Context,
}
#[cfg(any(feature = "log", feature = "tracing"))]
thread_local! {static ASSOCIATION:std::cell::RefCell<Option<Association>>=const{std::cell::RefCell::new(None)};}
#[cfg(any(feature = "log", feature = "tracing"))]
pub(crate) struct AssociationGuard {
    previous: Option<Association>,
    installed: bool,
}
#[cfg(any(feature = "log", feature = "tracing"))]
impl Drop for AssociationGuard {
    fn drop(&mut self) {
        if self.installed {
            ASSOCIATION.with(|slot| {
                if let Ok(mut value) = slot.try_borrow_mut() {
                    *value = self.previous.take();
                }
            });
        }
    }
}
#[cfg(any(feature = "log", feature = "tracing"))]
pub(crate) fn enter(association: Association) -> AssociationGuard {
    ASSOCIATION.with(|slot| {
        if let Ok(mut value) = slot.try_borrow_mut() {
            AssociationGuard {
                previous: value.replace(association),
                installed: true,
            }
        } else {
            AssociationGuard {
                previous: None,
                installed: false,
            }
        }
    })
}
#[cfg(any(feature = "log", feature = "tracing"))]
pub(crate) fn current(budget: &Arc<OperationBudget>) -> Option<Association> {
    ASSOCIATION.with(|slot| {
        slot.try_borrow()
            .ok()?
            .as_ref()
            .filter(|association| {
                association
                    .budget
                    .upgrade()
                    .is_some_and(|issuer| Arc::ptr_eq(&issuer, budget))
            })
            .cloned()
    })
}

#[cfg(test)]
mod tests;
