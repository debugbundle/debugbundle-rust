use super::{capture, Client};
use crate::{Context, ExceptionOccurrence};
use std::time::Instant;
impl Client {
    pub fn capture_occurrence(&self, occurrence: &ExceptionOccurrence, context: &Context) {
        self.enqueue(
            4,
            |state| {
                let Some(inner) = &self.inner else {
                    return false;
                };
                let context = capture::effective(inner, context);
                !context
                    .operation
                    .as_ref()
                    .and_then(|operation| operation.operation_id())
                    .is_some_and(|id| {
                        state
                            .occurrences
                            .contains(occurrence.id(), id, Instant::now())
                    })
            },
            |inner, state| capture::exception(inner, state, occurrence.snapshot(), context),
        );
    }
    /// Associate one protected occurrence with a synchronous native logging call.
    /// The supplied closure is the application's own business logging operation.
    #[cfg(any(feature = "log", feature = "tracing"))]
    pub fn with_error_log<T>(
        &self,
        occurrence: &ExceptionOccurrence,
        context: &Context,
        work: impl FnOnce() -> T,
    ) -> T {
        let _guard = self
            .inner
            .as_ref()
            .filter(|inner| {
                inner
                    .logging_enabled
                    .load(std::sync::atomic::Ordering::Acquire)
            })
            .map(|inner| {
                crate::occurrence::enter(crate::occurrence::Association {
                    budget: std::sync::Arc::downgrade(&inner.operations),
                    occurrence: occurrence.clone(),
                    context: capture::effective(inner, context),
                })
            });
        work()
    }
}
