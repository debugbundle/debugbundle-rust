use debugbundle::{Client, Config, Context, ExceptionInput, ExceptionSnapshot, TraceOrigin};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
#[error("application-owned storage failure")]
struct StorageFailure;

fn main() {
    let client = Client::new(Config::local("example-errors", ".debugbundle/local/events"));
    let context = Context::default();
    let opaque: anyhow::Error = StorageFailure.into();
    // Borrowed errors never execute Display, Debug or Error::source for telemetry.
    client.capture_error(ExceptionInput::borrowed(&*opaque), &context);
    let snapshot = ExceptionSnapshot::new("CheckoutFailure", "payment failed")
        .with_cause(&ExceptionSnapshot::new(
            "StorageFailure",
            "safe owned details",
        ))
        .with_stack(
            "0: checkout::authorize\n   at /srv/src/checkout.rs:42:7",
            TraceOrigin::RuntimeReported,
        );
    client.capture_exception(&snapshot, &context);
    client.shutdown(Duration::from_secs(2));
}
