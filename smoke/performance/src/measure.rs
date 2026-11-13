use debugbundle::{testkit::resources, Client};
use serde_json::{json, Value};
use std::time::Instant;

pub struct Measure {
    name: String,
    elapsed: Vec<u64>,
    allocations: u64,
    allocated_bytes: u64,
    caller_peak_bytes: u64,
    events: usize,
    bytes: usize,
    diagnostic_bytes: usize,
    windows: usize,
    operation_slots: usize,
    callback_workers: usize,
    client_slots: usize,
}
impl Measure {
    pub fn new(name: impl Into<String>, capacity: usize) -> Self {
        Self {
            name: name.into(),
            elapsed: Vec::with_capacity(capacity),
            allocations: 0,
            allocated_bytes: 0,
            caller_peak_bytes: 0,
            events: 0,
            bytes: 0,
            diagnostic_bytes: 0,
            windows: 0,
            operation_slots: 0,
            callback_workers: 0,
            client_slots: 0,
        }
    }
    pub fn call(&mut self, work: impl FnOnce()) {
        let start = Instant::now();
        let allocations = allocation_counter::measure(work);
        let elapsed = start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        self.elapsed.push(elapsed);
        self.allocations += allocations.count_total;
        self.allocated_bytes += allocations.bytes_total;
        self.caller_peak_bytes = self.caller_peak_bytes.max(allocations.bytes_max);
    }
    pub fn observe(&mut self, client: &Client) {
        if let Some(value) = resources(client) {
            assert!(value.buffered_events <= 1000 && value.buffered_bytes <= 8 * 1024 * 1024);
            assert!(value.diagnostic_bytes <= 65536 && value.diagnostic_windows <= 50);
            assert!(value.operation_slots <= 1024 && value.operation_metadata_bytes <= 262144);
            assert!(value.process_client_slots <= 8 && value.process_callback_workers <= 8);
            self.events = self.events.max(value.buffered_events);
            self.bytes = self.bytes.max(value.buffered_bytes);
            self.diagnostic_bytes = self.diagnostic_bytes.max(value.diagnostic_bytes);
            self.windows = self.windows.max(value.diagnostic_windows);
            self.operation_slots = self.operation_slots.max(value.operation_slots);
            self.callback_workers = self.callback_workers.max(value.process_callback_workers);
            self.client_slots = self.client_slots.max(value.process_client_slots);
        }
    }
    pub fn finish(mut self) -> Value {
        self.elapsed.sort_unstable();
        assert!(!self.elapsed.is_empty());
        let percentile =
            |p: usize| self.elapsed[(self.elapsed.len() * p).div_ceil(100).saturating_sub(1)];
        let total: u128 = self.elapsed.iter().map(|n| u128::from(*n)).sum();
        json!({"case":self.name,"samples":self.elapsed.len(),"p50_ns":percentile(50),"p95_ns":percentile(95),"p99_ns":percentile(99),"instrumented_calls_per_second":self.elapsed.len() as f64 * 1e9 / total as f64,"caller_allocations":self.allocations,"caller_allocated_bytes":self.allocated_bytes,"caller_peak_allocated_bytes_per_call":self.caller_peak_bytes,"observed_peak_event_count":self.events,"observed_peak_event_bytes":self.bytes,"observed_peak_joint_diagnostic_bytes":self.diagnostic_bytes,"observed_peak_diagnostic_windows":self.windows,"observed_peak_operation_slots":self.operation_slots,"observed_peak_process_callback_workers":self.callback_workers,"observed_peak_process_client_slots":self.client_slots})
    }
}
