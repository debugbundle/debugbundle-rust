use debugbundle::{Client, Config, ExceptionSnapshot, OperationKind, OperationOptions};
use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(Config::from_env());
    let mut operation = client.start_operation(OperationOptions {
        kind: OperationKind::Job,
        name: "example.job".into(),
        limit: Some(Duration::from_secs(10)),
        ..Default::default()
    });
    let context = operation.context();
    let task_client = client.clone();
    tokio::spawn(operation.handle().wrap(async move {
        task_client.capture_exception(
            &ExceptionSnapshot::new("ExampleFailure", "synthetic job failed"),
            &context,
        );
    }))
    .await?;
    operation.stop();
    client.shutdown_async(Duration::from_secs(2)).await;
    Ok(())
}
