//! Composable native logging with bounded primitives and no custom formatting.
use crate::Level;
use serde_json::Value;
pub(crate) struct NativeLog {
    pub level: Level,
    pub message: String,
    pub attributes: Value,
    pub metadata: Value,
    pub native_error: bool,
}
thread_local! {static ENTERED:std::cell::Cell<bool>=const{std::cell::Cell::new(false)};}
pub(crate) fn entered() -> bool {
    ENTERED.with(std::cell::Cell::get)
}
struct Entered;
impl Drop for Entered {
    fn drop(&mut self) {
        ENTERED.with(|flag| flag.set(false));
    }
}
fn enter() -> Option<Entered> {
    ENTERED.with(|flag| {
        // Only an admitted call owns a guard. Eagerly constructing then dropping
        // one on rejection would clear the outer call's recursion protection.
        if flag.replace(true) {
            None
        } else {
            Some(Entered)
        }
    })
}
#[cfg(feature = "log")]
mod log_adapter;
#[cfg(feature = "log")]
pub use log_adapter::{InstallStatus, Logger};
#[cfg(feature = "tracing")]
pub(crate) mod spans;
#[cfg(feature = "tracing")]
mod tracing_layer;

#[cfg(test)]
mod tests {
    use super::{enter, entered};

    #[test]
    fn rejected_recursive_logs_preserve_the_outer_guard() {
        let outer = enter().expect("outer logging call enters");
        for _ in 0..2 {
            assert!(entered());
            assert!(enter().is_none());
            assert!(
                entered(),
                "rejection must not release the outer logging guard"
            );
        }
        drop(outer);
        assert!(!entered());
        assert!(enter().is_some());
        assert!(!entered());
    }
}
