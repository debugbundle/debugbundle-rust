#![cfg(all(feature = "local", unix))]
use debugbundle::{storage::SecureDirectory, Client, Config, Context, Level};
use serde_json::{json, Value};
use std::{
    net::TcpListener,
    path::PathBuf,
    time::{Duration, Instant},
};

#[test]
#[ignore = "requires isolated 64-KiB tmpfs; run make test-storage-faults"]
fn disk_full_short_writes_publish_no_arrays_and_retain_local_ownership_without_http_fallback() {
    let root = PathBuf::from(
        std::env::var_os("DEBUGBUNDLE_STORAGE_FAULT_DIR").expect("bounded tmpfs fixture required"),
    );
    let directory = SecureDirectory::open(&root).unwrap();
    let events=(0..8).map(|index|json!({"schema_version":"2026-03-01","event_id":uuid::Uuid::new_v4().to_string(),"event_type":"log_event","sdk_name":"@debugbundle/sdk-rust","sdk_version":"1.0.0-alpha.1","service":{"name":"disk-fault","environment":"test","runtime":"rust"},"occurred_at":"2026-11-13T00:00:00.000Z","payload":{"level":"error","message":format!("synthetic-{index} {}","x".repeat(15000)),"attributes":{}}})).collect::<Vec<Value>>();
    assert!(serde_json::to_vec(&events).unwrap().len() > 100000);
    let error = directory.write_events(&events, "disk-fault").unwrap_err();
    assert_eq!(
        error.raw_os_error(),
        Some(28),
        "fixture must fail with real ENOSPC"
    );
    assert_eq!(
        root.read_dir().unwrap().count(),
        0,
        "no partial arrays or temporary files"
    );

    let trap = TcpListener::bind("127.0.0.1:0").unwrap();
    trap.set_nonblocking(true).unwrap();
    let client = Client::new(Config {
        endpoint: format!("http://{}/v1/events", trap.local_addr().unwrap()),
        allow_insecure_http: true,
        batch_size: 100,
        flush_interval: Duration::from_secs(300),
        ..Config::local("disk-fault", &root)
    });
    for prefix in [
        "Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel",
    ] {
        client.capture_log(
            Level::Error,
            &format!("{prefix} {}", "x".repeat(15000)),
            &Context::default(),
        );
    }
    assert_eq!(client.status().buffered_events, 8);
    let start = Instant::now();
    let outcome = client.flush(Duration::from_millis(100));
    assert!(!outcome.drained && outcome.timed_out);
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(client.status().buffered_events, 8);
    assert_eq!(root.read_dir().unwrap().count(), 0);
    assert_eq!(
        trap.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    client.shutdown(Duration::from_millis(100));
}
