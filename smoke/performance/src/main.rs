mod capture;
mod frameworks;
mod measure;
mod native;
mod support;
use serde_json::json;

fn main() {
    let mut cases = Vec::new();
    for mode in ["disabled", "baseline", "evidence"] {
        cases.extend(capture::ordinary(mode));
        cases.extend(frameworks::run(mode));
    }
    cases.extend(capture::pressure("hook"));
    cases.extend(capture::pressure("transport"));
    cases.extend(native::pressure());
    cases.push(capture::ledger_pressure());
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let report = json!({"schema_version":1,"architecture":std::env::consts::ARCH,"profile":"release","samples_per_capture_case":support::SAMPLES,"instrumentation":"current-thread allocation-counter 0.8.1; timings include its overhead; setup, queue seeding, drain and snapshot reads excluded","heap_scope":"caller allocation peaks and sampled SDK allowances are distinct from whole-process RSS; no total-heap cap inferred","process_memory_status":status.lines().filter(|line|line.starts_with("VmRSS:")||line.starts_with("VmHWM:")||line.starts_with("Threads:")).collect::<Vec<_>>(),"cases":cases});
    let output = std::env::args().nth(1).expect("benchmark output path");
    std::fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    println!(
        "Measured {} bounded capture/framework/pressure cases",
        report["cases"].as_array().unwrap().len()
    );
}
