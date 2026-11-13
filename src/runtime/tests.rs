#[tokio::test]
async fn native_resolver_workers_are_marked_internal_before_any_logging() {
    let runtime = super::handle().unwrap();
    assert!(
        runtime.spawn_blocking(super::internal).await.unwrap(),
        "SDK-owned blocking workers must not feed SDK logging back into capture"
    );
    assert!(
        !super::internal(),
        "the application executor must retain its own thread state"
    );
    assert!(runtime
        .spawn_blocking(|| std::thread::current()
            .name()
            .unwrap_or("")
            .starts_with("debugbundle-native"))
        .await
        .unwrap());
}
