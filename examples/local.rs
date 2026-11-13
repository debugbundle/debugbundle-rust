use debugbundle::{Client, Config, Context, ExceptionSnapshot};
use std::time::Duration;

fn main() {
    let client = Client::new(Config::local(
        "example-command",
        ".debugbundle/local/events",
    ));
    client.capture_exception(
        &ExceptionSnapshot::new("ExampleFailure", "synthetic command failed"),
        &Context::default(),
    );
    let result = client.shutdown(Duration::from_secs(2));
    println!(
        "local accepted={}, drained={}",
        result.accepted_events, result.drained
    );
}
