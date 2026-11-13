use crate::{measure::Measure, support::*};
use debugbundle::{
    Client, Context, ExceptionInput, OperationOptions, ProbeOptions, RecoveryObservation,
    RecoveryState,
};
use log::Log;
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};
use tracing_subscriber::prelude::*;

pub fn ordinary(mode: &str) -> Vec<Value> {
    let destination = sink(None);
    let client = Client::new(config(mode, destination.clone()));
    let context = Context::default();
    let values = snapshots(SAMPLES + 1);
    client.capture_exception(&values[SAMPLES], &context);
    drain(&client);
    let before = destination.accepted.load(Ordering::Relaxed);
    let mut measure = Measure::new(format!("capture/{mode}"), SAMPLES);
    for snapshot in &values[..SAMPLES] {
        measure.call(|| std::hint::black_box(&client).capture_exception(snapshot, &context));
        measure.observe(&client);
        drain(&client);
    }
    assert_eq!(
        destination.accepted.load(Ordering::Relaxed) - before,
        if mode == "disabled" { 0 } else { SAMPLES }
    );
    let mut results = vec![measure.finish()];
    let subscriber = tracing_subscriber::registry().with(client.tracing_layer());
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let mut measure = Measure::new(format!("tracing/{mode}"), SAMPLES);
    for (index, value) in values[..SAMPLES].iter().enumerate() {
        measure.call(|| tracing::error!(message = value.name(), technical = index as u64));
        measure.observe(&client);
        drain(&client);
    }
    results.push(measure.finish());
    let logger = client.logger();
    let mut measure = Measure::new(format!("log/{mode}"), 32);
    for index in 0..32 {
        measure.call(|| static_log(&logger, index % 16));
        measure.observe(&client);
        drain(&client);
    }
    results.push(measure.finish());
    client.shutdown(Duration::from_secs(2));
    results
}
fn static_log(logger: &impl log::Log, index: usize) {
    macro_rules! record {
        ($message:literal) => {
            logger.log(
                &log::Record::builder()
                    .level(log::Level::Error)
                    .args(format_args!($message))
                    .build(),
            )
        };
    }
    match index {
        0 => record!("synthetic alpha"),
        1 => record!("synthetic bravo"),
        2 => record!("synthetic charlie"),
        3 => record!("synthetic delta"),
        4 => record!("synthetic echo"),
        5 => record!("synthetic foxtrot"),
        6 => record!("synthetic golf"),
        7 => record!("synthetic hotel"),
        8 => record!("synthetic india"),
        9 => record!("synthetic juliet"),
        10 => record!("synthetic kilo"),
        11 => record!("synthetic lima"),
        12 => record!("synthetic mike"),
        13 => record!("synthetic november"),
        14 => record!("synthetic oscar"),
        _ => record!("synthetic papa"),
    }
}
struct Opaque(Arc<AtomicUsize>);
impl std::fmt::Debug for Opaque {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fetch_add(1, Ordering::Relaxed);
        panic!("formatter must not execute")
    }
}
impl std::fmt::Display for Opaque {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fetch_add(1, Ordering::Relaxed);
        panic!("formatter must not execute")
    }
}
impl std::error::Error for Opaque {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.fetch_add(1, Ordering::Relaxed);
        panic!("source must not execute")
    }
}

pub fn pressure(stage: &str) -> Vec<Value> {
    let (gate, started) = Gate::new();
    let destination = sink((stage == "transport").then(|| gate.clone()));
    let mut configuration = config("evidence", destination);
    configuration.batch_size = 1;
    configuration.max_buffered_events = 1;
    if stage == "hook" {
        let gate = gate.clone();
        configuration.before_send = Some(Arc::new(move |event| {
            gate.wait();
            Some(event)
        }));
    }
    let client = Client::new(configuration);
    pressure_calls(stage, client, gate, started)
}

pub fn pressure_calls(
    stage: &str,
    client: Client,
    gate: Arc<Gate>,
    started: mpsc::Receiver<()>,
) -> Vec<Value> {
    let context = Context::default();
    client.capture_exception(&snapshots(1)[0], &context);
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    let formatters = Arc::new(AtomicUsize::new(0));
    let opaque = Opaque(formatters.clone());
    let producers = Arc::new(AtomicUsize::new(0));
    let counter = producers.clone();
    let producer = Arc::new(move || {
        counter.fetch_add(1, Ordering::Relaxed);
        json!({})
    });
    let mut measure = Measure::new(format!("pressure-{stage}/capture-and-lazy"), SAMPLES);
    for _ in 0..SAMPLES {
        measure.call(|| {
            client.capture_error(ExceptionInput::borrowed(&opaque), &context);
            client.probe_lazy("pressure", &producer, ProbeOptions::default(), &context);
        });
        measure.observe(&client);
    }
    let mut results = vec![measure.finish()];
    let subscriber = tracing_subscriber::registry().with(client.tracing_layer());
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let logger = client.logger();
    let mut measure = Measure::new(format!("pressure-{stage}/tracing-and-log"), SAMPLES);
    for _ in 0..SAMPLES {
        measure.call(|| {
            tracing::error!(message="full queue", opaque=?opaque);
            logger.log(
                &log::Record::builder()
                    .level(log::Level::Error)
                    .args(format_args!("{opaque}"))
                    .build(),
            );
        });
        measure.observe(&client);
    }
    results.push(measure.finish());
    assert_eq!(formatters.load(Ordering::Relaxed), 0);
    assert_eq!(producers.load(Ordering::Relaxed), 0);
    assert_eq!(gate.calls.load(Ordering::Relaxed), 1);
    assert!(
        !gate.expired.load(Ordering::Acquire),
        "capture waited for the stalled application callback"
    );
    gate.release();
    drain(&client);
    client.shutdown(Duration::from_secs(2));
    results
}

pub fn ledger_pressure() -> Value {
    let (gate, started) = Gate::new();
    let mut configuration = config("evidence", sink(Some(gate.clone())));
    configuration.batch_size = 1;
    configuration.capture_recovery_observations = true;
    let client = Client::new(configuration);
    client.capture_exception(&snapshots(1)[0], &Context::default());
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    let operation = client.start_operation(OperationOptions::default());
    let mut measure = Measure::new("pressure/diagnostic-ledger", SAMPLES);
    let id = |n: usize| format!("00000000-0000-4000-8000-{n:012x}");
    for failure in 1..=50 {
        let context = operation.context().with_failure_ref(&id(failure));
        for attempt in 0..5 {
            for state in [
                RecoveryState::AttemptStarted,
                RecoveryState::AttemptCompleted,
                RecoveryState::RecoveryObserved,
                RecoveryState::FailurePersisted,
            ] {
                client.capture_recovery(
                    &RecoveryObservation {
                        failure_ref: id(failure),
                        attempt_id: id(1000 + failure * 5 + attempt),
                        state,
                        ..Default::default()
                    },
                    &context,
                );
                measure.observe(&client);
            }
        }
    }
    let context = operation.context().with_failure_ref(&id(1));
    let observation = RecoveryObservation {
        failure_ref: id(1),
        attempt_id: id(9999),
        state: RecoveryState::AttemptStarted,
        ..Default::default()
    };
    for _ in 0..SAMPLES {
        measure.call(|| client.capture_recovery(&observation, &context));
        measure.observe(&client);
    }
    assert!(!gate.expired.load(Ordering::Acquire));
    gate.release();
    drain(&client);
    client.shutdown(Duration::from_secs(2));
    measure.finish()
}
