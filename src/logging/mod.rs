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
    ENTERED.with(|flag| (!flag.replace(true)).then_some(Entered))
}
#[cfg(feature = "log")]
mod log_adapter;
#[cfg(feature = "log")]
pub use log_adapter::{InstallStatus, Logger};
#[cfg(feature = "tracing")]
pub(crate) mod spans;
#[cfg(feature = "tracing")]
mod tracing_layer;
