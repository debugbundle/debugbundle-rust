use super::*;
use std::sync::{Arc, Barrier};
#[test]
fn contended_file_leases_are_retryable_and_reacquired_after_release() {
    if !cfg!(any(target_os = "linux", target_os = "macos", windows)) {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let directory = SecureDirectory::open(&temp.path().join("leases")).unwrap();
    let lease = directory.lock("record.lease").unwrap();
    assert_eq!(
        directory.lock("record.lease").unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(lease);
    let lease = directory.lock("record.lease").unwrap();
    drop(lease);
    directory.remove("record.lease", &[]).unwrap();
}

#[test]
fn concurrent_publication_never_overwrites_an_owned_destination() {
    if !cfg!(any(target_os = "linux", target_os = "macos", windows)) {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let directory = Arc::new(SecureDirectory::open(&temp.path().join("events")).unwrap());
    let start = Arc::new(Barrier::new(16));
    let workers = (0..16)
        .map(|index| {
            let directory = directory.clone();
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                directory
                    .write_named("one.events.json", format!("[{index}]").as_bytes())
                    .is_ok()
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|success| *success)
            .count(),
        1
    );
    let original = directory.read("one.events.json").unwrap();
    assert!(directory.write_named("one.events.json", b"[]").is_err());
    assert_eq!(directory.read("one.events.json").unwrap(), original);
}
