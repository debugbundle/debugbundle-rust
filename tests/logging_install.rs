#![cfg(feature = "log")]
mod helpers;
use debugbundle::{logging::InstallStatus, Client};
use helpers::{configured, Recorder};
use log::Log;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

struct ApplicationSink {
    records: Arc<AtomicUsize>,
    flushes: Arc<AtomicUsize>,
    recursive: bool,
}
impl Log for ApplicationSink {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, _: &log::Record<'_>) {
        self.records.fetch_add(1, Ordering::SeqCst);
        if self.recursive {
            log::error!("recursive application bridge");
        }
    }
    fn flush(&self) {
        self.flushes.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn global_logger_composition_preserves_existing_installation_and_bounds_recursion() {
    for mode in ["existing", "installed", "bridge"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "logging_install_child", "--nocapture"])
            .env("DEBUGBUNDLE_LOG_INSTALL_CHILD", mode)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn logging_install_child() {
    let Ok(mode) = std::env::var("DEBUGBUNDLE_LOG_INSTALL_CHILD") else {
        return;
    };
    let recorder = Arc::new(Recorder::default());
    let client = Client::new(configured(recorder.clone()));
    let records = Arc::new(AtomicUsize::new(0));
    let flushes = Arc::new(AtomicUsize::new(0));
    let sink = Arc::new(ApplicationSink {
        records: records.clone(),
        flushes: flushes.clone(),
        recursive: mode == "bridge",
    });
    if mode == "existing" {
        log::set_boxed_logger(Box::new(sink)).unwrap();
        log::set_max_level(log::LevelFilter::Info);
        assert_eq!(
            client.logger().try_install(),
            InstallStatus::AlreadyInstalled
        );
        assert_eq!(log::max_level(), log::LevelFilter::Info);
        log::info!("existing application logger");
        assert_eq!(records.load(Ordering::SeqCst), 1);
        assert!(client.flush(Duration::from_secs(2)).drained);
        assert!(recorder.events.lock().unwrap().is_empty());
        return;
    }
    assert_eq!(
        client.logger().tee(sink).try_install(),
        InstallStatus::Installed
    );
    for level in [
        log::Level::Trace,
        log::Level::Debug,
        log::Level::Info,
        log::Level::Warn,
        log::Level::Error,
    ] {
        assert!(log::logger().enabled(&log::Metadata::builder().level(level).build()));
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut attempts = 0;
    loop {
        // SDK admission is nonblocking/best effort while the sender owns its
        // state mutex. Application logging must run exactly once per attempt,
        // even when that optional SDK admission is declined.
        log::error!("native error through application sink");
        attempts += 1;
        assert!(client.flush(Duration::from_secs(2)).drained);
        if !recorder.events.lock().unwrap().is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native logger never admitted a record"
        );
        std::thread::yield_now();
    }
    assert_eq!(records.load(Ordering::SeqCst), attempts);
    assert_eq!(recorder.events.lock().unwrap().len(), 1);
    client.detach_logging();
    log::warn!("application logging remains after detach");
    log::logger().flush();
    assert_eq!(records.load(Ordering::SeqCst), attempts + 1);
    assert_eq!(flushes.load(Ordering::SeqCst), 1);
    assert!(client.flush(Duration::from_secs(2)).drained);
    assert_eq!(recorder.events.lock().unwrap().len(), 1);
    assert_eq!(
        client.logger().try_install(),
        InstallStatus::AlreadyInstalled
    );
}
