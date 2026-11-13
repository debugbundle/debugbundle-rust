use super::{enter, entered, NativeLog};
use crate::{Client, Level};
use serde_json::json;
use std::sync::Arc;
pub struct Logger {
    client: Client,
    next: Option<Arc<dyn log::Log>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallStatus {
    Installed,
    AlreadyInstalled,
}
impl Client {
    pub fn logger(&self) -> Logger {
        Logger {
            client: self.clone(),
            next: None,
        }
    }
}
fn level(level: log::Level) -> Level {
    match level {
        log::Level::Error => Level::Error,
        log::Level::Warn => Level::Warning,
        log::Level::Info => Level::Info,
        log::Level::Debug => Level::Debug,
        log::Level::Trace => Level::Trace,
    }
}
impl Logger {
    /// Explicitly compose the application's existing sink. Its own callbacks and
    /// filtering retain their native logging semantics.
    pub fn tee(mut self, next: Arc<dyn log::Log>) -> Self {
        self.next = Some(next);
        self
    }
    pub fn try_install(self) -> InstallStatus {
        if log::set_boxed_logger(Box::new(self)).is_err() {
            return InstallStatus::AlreadyInstalled;
        }
        log::set_max_level(log::LevelFilter::Trace);
        InstallStatus::Installed
    }
    // Keep the disabled/no-sink path out of the application callbacks, recursion
    // guard and record-building stack frame. Tee sinks still receive their own
    // filtering and callbacks even when SDK capture is disabled.
    #[inline(never)]
    fn forward_record(&self, record: &log::Record<'_>) {
        let Some(_guard) = enter() else {
            return;
        };
        if let Some(next) = &self.next {
            if next.enabled(record.metadata()) {
                next.log(record);
            }
        }
        if !self.client.native_logging_enabled(level(record.level())) {
            return;
        }
        self.client.capture_native_log(&NativeLog{level:level(record.level()),message:crate::model::text(record.args().as_str().unwrap_or("Log message unavailable without formatting"),16384),attributes:json!({}),metadata:json!({"system":"log","target":crate::model::text(record.target(),256),"module":record.module_path().map(|value|crate::model::text(value,256)),"file":record.file().map(|value|crate::model::text(value,2048)),"line":record.line(),"formatting_available":record.args().as_str().is_some()}),native_error:false});
    }
}
impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        !entered()
            && (self
                .next
                .as_ref()
                .is_some_and(|next| next.enabled(metadata))
                || self.client.native_logging_enabled(level(metadata.level())))
    }
    #[inline]
    fn log(&self, record: &log::Record<'_>) {
        if self.next.is_none() && !self.client.native_logging_enabled(level(record.level())) {
            return;
        }
        self.forward_record(record);
    }
    fn flush(&self) {
        if let Some(next) = &self.next {
            next.flush();
        }
    }
}
