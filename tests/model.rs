use debugbundle::{Context, ExceptionInput, ExceptionSnapshot, TraceOrigin};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
static CALLS: AtomicUsize = AtomicUsize::new(0);
struct HostError;
impl std::fmt::Debug for HostError {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        CALLS.fetch_add(1, Ordering::Relaxed);
        panic!("must not debug-format")
    }
}
impl std::fmt::Display for HostError {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        CALLS.fetch_add(1, Ordering::Relaxed);
        panic!("must not format")
    }
}
impl std::error::Error for HostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        panic!("must not inspect")
    }
}

#[test]
fn borrowed_errors_never_execute_host_accessors() {
    let snapshot = ExceptionInput::borrowed(&HostError).snapshot();
    assert!(snapshot.name().contains("HostError"));
    assert_eq!(CALLS.load(Ordering::Relaxed), 0);
    assert_eq!(snapshot.evidence()["unavailable_reason"], "unsafe");
}

#[test]
fn safe_snapshots_preserve_causes_and_honest_trace_origins() {
    let cause = ExceptionSnapshot::new("StorageError", "password=SYNTHETIC_SECRET");
    let snapshot = ExceptionSnapshot::new("CheckoutError", "failed")
        .with_cause(&cause)
        .with_stack(
            "0: checkout::run\n at /srv/src/main.rs:42:7",
            TraceOrigin::RuntimeReported,
        );
    assert_eq!(snapshot.evidence()["runtime"], "unknown");
    assert_eq!(
        snapshot.evidence()["nodes"][0]["trace_origin"],
        "runtime_reported"
    );
    assert_eq!(snapshot.evidence()["nodes"].as_array().unwrap().len(), 2);
    assert!(!serde_json::to_string(snapshot.evidence())
        .unwrap()
        .contains("SYNTHETIC_SECRET"));
}

#[test]
fn aggregate_snapshots_preserve_nested_safe_graphs_within_contract_budgets() {
    let cause = ExceptionSnapshot::new("Cause", "password=SYNTHETIC_SECRET");
    let branch = ExceptionSnapshot::new("Branch", "nested failure")
        .with_cause(&cause)
        .with_aggregate(&[ExceptionSnapshot::new("Sibling", "independent failure")]);
    let child = ExceptionSnapshot::new("Child", "group").with_aggregate(&[branch]);
    let snapshot = ExceptionSnapshot::new("Root", "primary").with_aggregate(&[child]);
    assert_canonical_snapshot(&snapshot);
    let graph = snapshot.evidence();
    assert_eq!(graph["nodes"].as_array().unwrap().len(), 5);
    assert_eq!(graph["truncated"], false);
    assert_eq!(graph["unavailable_reason"], Value::Null);
    assert_eq!(
        graph["nodes"][0]["relationships"],
        json!([{"kind":"aggregate","target_id":1}])
    );
    assert_eq!(
        graph["nodes"][1]["relationships"],
        json!([{"kind":"aggregate","target_id":2}])
    );
    assert_eq!(
        graph["nodes"][2]["relationships"],
        json!([
            {"kind":"cause","target_id":3}, {"kind":"aggregate","target_id":4}
        ])
    );
    assert_eq!(graph["nodes"][3]["name"], "Cause");
    assert!(!graph.to_string().contains("SYNTHETIC_SECRET"));
}

#[test]
fn aggregate_depth_exhaustion_preserves_earlier_edges_without_fabricating_completeness() {
    let mut child = ExceptionSnapshot::new("Leaf", "leaf");
    for _ in 0..4 {
        child = ExceptionSnapshot::new("Branch", "branch").with_cause(&child);
    }
    let snapshot = ExceptionSnapshot::new("Root", "primary")
        .with_cause(&ExceptionSnapshot::new("OriginalCause", "keep"))
        .with_aggregate(&[child]);
    assert_canonical_snapshot(&snapshot);
    assert_eq!(snapshot.evidence()["nodes"][1]["name"], "OriginalCause");
    assert_eq!(snapshot.evidence()["truncated"], true);
    assert_eq!(snapshot.evidence()["unavailable_reason"], "budget_exceeded");
}

#[test]
fn repeated_snapshot_causes_are_bounded_without_erasing_the_primary() {
    let mut snapshot = ExceptionSnapshot::new("Root", "failure");
    for _ in 0..20 {
        snapshot = ExceptionSnapshot::new("Outer", "outer failure").with_cause(&snapshot);
    }
    assert!(snapshot.evidence()["nodes"].as_array().unwrap().len() <= 8);
    assert_eq!(snapshot.name(), "Outer");
    assert_eq!(snapshot.evidence()["truncated"], true);
}

#[test]
fn context_is_an_owned_protected_primitive_map() {
    let mut input = json!({"token":"SYNTHETIC_SECRET","route":"/checkout"});
    let context = Context::new(&input);
    input["route"] = Value::String("/changed".into());
    assert_eq!(
        context.as_value(),
        &json!({"token":"[REDACTED]","route":"/checkout"})
    );
}

#[test]
fn supplied_frames_are_protected_and_bounded_without_inventing_a_throw_site() {
    let frame = debugbundle::FrameSnapshot {
        function: Some("checkout::run password=SYNTHETIC_SECRET".into()),
        file: Some("/srv/src/main.rs".into()),
        line: Some(42),
        column: Some(7),
        ..Default::default()
    };
    let snapshot = ExceptionSnapshot::new("Failure", "failed")
        .with_frames(&vec![frame; 40], TraceOrigin::RuntimeReported);
    assert_eq!(
        snapshot.evidence()["nodes"][0]["frames"]
            .as_array()
            .unwrap()
            .len(),
        32
    );
    assert_eq!(
        snapshot.evidence()["nodes"][0]["trace_origin"],
        "runtime_reported"
    );
    assert_eq!(snapshot.evidence()["truncated"], true);
    assert!(!serde_json::to_string(snapshot.evidence())
        .unwrap()
        .contains("SYNTHETIC_SECRET"));
}

#[test]
fn aggregate_snapshots_bound_children_and_preserve_owned_protected_relationships() {
    let child = ExceptionSnapshot::new("Inner", "password=SYNTHETIC_SECRET")
        .with_cause(&ExceptionSnapshot::new("Cause", "synthetic"));
    let snapshot =
        ExceptionSnapshot::new("Aggregate", "many failures").with_aggregate(&vec![child; 10]);
    let graph = snapshot.evidence();
    assert_canonical_snapshot(&snapshot);
    assert_eq!(graph["nodes"].as_array().unwrap().len(), 7);
    assert_eq!(
        graph["nodes"][0]["relationships"].as_array().unwrap().len(),
        3
    );
    assert_eq!(graph["truncated"], true);
    for (index, nodes) in graph["nodes"].as_array().unwrap()[1..]
        .as_chunks::<2>()
        .0
        .iter()
        .enumerate()
    {
        assert_eq!(
            nodes[0]["relationships"],
            json!([{"kind":"cause","target_id":2 + index * 2}])
        );
        assert_eq!(nodes[1]["relationships"], json!([]));
    }
    assert!(!serde_json::to_string(graph)
        .unwrap()
        .contains("SYNTHETIC_SECRET"));
}

fn assert_canonical_snapshot(snapshot: &ExceptionSnapshot) {
    let corpus: Value =
        serde_json::from_str(include_str!("fixtures/evidence-wire-cases.json")).unwrap();
    let mut event = corpus["events"][0]["event"].clone();
    event["payload"]["exception_evidence"] = snapshot.evidence().clone();
    assert!(
        debugbundle::wire::valid_event(&event),
        "{}",
        snapshot.evidence()
    );
}

#[test]
fn cause_and_aggregate_composition_preserves_existing_reachable_evidence_in_either_order() {
    let cause = ExceptionSnapshot::new("Cause", "causal failure");
    let child = ExceptionSnapshot::new("Child", "aggregate member");
    for snapshot in [
        ExceptionSnapshot::new("Root", "primary")
            .with_cause(&cause)
            .with_aggregate(std::slice::from_ref(&child)),
        ExceptionSnapshot::new("Root", "primary")
            .with_aggregate(std::slice::from_ref(&child))
            .with_cause(&cause),
    ] {
        assert_canonical_snapshot(&snapshot);
        assert_eq!(snapshot.evidence()["nodes"].as_array().unwrap().len(), 3);
        assert_eq!(snapshot.evidence()["truncated"], false);
        let edges = snapshot.evidence()["nodes"][0]["relationships"]
            .as_array()
            .unwrap();
        assert!(edges.iter().any(|edge| edge["kind"] == "cause"));
        assert!(edges.iter().any(|edge| edge["kind"] == "aggregate"));
    }
}

#[test]
fn repeated_aggregate_and_cause_builders_never_orphan_nodes_or_exceed_edge_budgets() {
    let child = ExceptionSnapshot::new("Child", "aggregate member");
    let mut snapshot = ExceptionSnapshot::new("Root", "primary")
        .with_cause(&ExceptionSnapshot::new("OriginalCause", "causal failure"));
    for _ in 0..12 {
        snapshot = snapshot
            .with_aggregate(std::slice::from_ref(&child))
            .with_cause(&ExceptionSnapshot::new("LaterCause", "must not replace"));
        assert_canonical_snapshot(&snapshot);
    }
    let nodes = snapshot.evidence()["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 6);
    assert_eq!(nodes[1]["name"], "OriginalCause");
    assert_eq!(snapshot.evidence()["truncated"], true);
    assert_eq!(snapshot.evidence()["unavailable_reason"], "budget_exceeded");
}

#[test]
fn aggregate_byte_pressure_retains_existing_graph_and_reports_omitted_children() {
    let large = ExceptionSnapshot::new("Large", "child")
        .with_stack(&"x".repeat(16384), TraceOrigin::RuntimeReported);
    let snapshot = ExceptionSnapshot::new("Root", "primary")
        .with_cause(&ExceptionSnapshot::new("Cause", "causal failure"))
        .with_aggregate(&[large.clone(), large]);
    assert_canonical_snapshot(&snapshot);
    assert_eq!(snapshot.evidence()["nodes"].as_array().unwrap().len(), 3);
    assert_eq!(snapshot.evidence()["truncated"], true);
    assert_eq!(snapshot.evidence()["unavailable_reason"], "budget_exceeded");
}

#[test]
fn composed_graphs_preserve_unsafe_child_status_alongside_budget_truncation() {
    let opaque = ExceptionInput::borrowed(&HostError).snapshot();
    for snapshot in [
        ExceptionSnapshot::new("Root", "primary").with_cause(&opaque),
        ExceptionSnapshot::new("Root", "primary").with_aggregate(std::slice::from_ref(&opaque)),
        opaque.clone().with_frames(
            &vec![debugbundle::FrameSnapshot::default(); 40],
            TraceOrigin::RuntimeReported,
        ),
        opaque.with_frames(
            &vec![
                debugbundle::FrameSnapshot {
                    file: Some("x".repeat(2048)),
                    ..Default::default()
                };
                32
            ],
            TraceOrigin::RuntimeReported,
        ),
    ] {
        assert_canonical_snapshot(&snapshot);
        assert_eq!(snapshot.evidence()["unavailable_reason"], "unsafe");
    }
    assert_eq!(CALLS.load(Ordering::Relaxed), 0);
}

#[test]
fn graph_field_byte_limits_report_truncation_without_losing_primary_metadata() {
    for snapshot in [
        ExceptionSnapshot::new(&"é".repeat(129), "primary"),
        ExceptionSnapshot::new("Root", &"é".repeat(1025)),
        ExceptionSnapshot::new("Root", "primary")
            .with_stack(&"x".repeat(16385), TraceOrigin::RuntimeReported),
        ExceptionSnapshot::new("Root", "primary").with_frames(
            &[debugbundle::FrameSnapshot {
                function: Some("é".repeat(129)),
                ..Default::default()
            }],
            TraceOrigin::RuntimeReported,
        ),
        ExceptionSnapshot::new("Root", "primary").with_frames(
            &[debugbundle::FrameSnapshot {
                file: Some("é".repeat(1025)),
                ..Default::default()
            }],
            TraceOrigin::RuntimeReported,
        ),
    ] {
        assert_canonical_snapshot(&snapshot);
        assert_eq!(
            snapshot.evidence()["truncated"],
            true,
            "{}",
            snapshot.evidence()
        );
        assert_eq!(snapshot.evidence()["unavailable_reason"], "budget_exceeded");
        assert!(!snapshot.name().is_empty());
        assert!(!snapshot.message().is_empty());
    }
}

#[test]
fn public_bootstrap_and_correlation_helpers_preserve_opaque_valid_identifiers() {
    let id = debugbundle::new_evidence_id().unwrap();
    let context = Context::default().with_session_id(&id);
    let client = debugbundle::init(debugbundle::Config {
        enabled: false,
        ..Default::default()
    });
    client.capture_exception(&ExceptionSnapshot::new("Synthetic", "inactive"), &context);
    assert_eq!(client.status().reason, Some("disabled"));
    assert_eq!(client.status().buffered_events, 0);
    assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(), 4);
}

#[test]
fn mutated_snapshot_sequences_preserve_canonical_evidence() {
    let corpus: Value =
        serde_json::from_str(include_str!("fixtures/snapshot-regressions.json")).unwrap();
    let base: Value =
        serde_json::from_str(include_str!("fixtures/evidence-wire-cases.json")).unwrap();
    for input in corpus["inputs"].as_array().unwrap() {
        let bytes: Vec<u8> = serde_json::from_value(input.clone()).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        // Retain the fuzz target's type-name width: changing it moves this
        // control-character-heavy graph away from the serialized byte boundary.
        let child = ExceptionSnapshot::new(
            "snapshots::Opaque",
            "Error details unavailable without a safe snapshot",
        )
        .with_stack(&text, TraceOrigin::RuntimeReported);
        let mut snapshot = ExceptionSnapshot::new(&text, &text);
        for (step, byte) in bytes.iter().take(16).enumerate() {
            snapshot = match byte % 4 {
                0 => snapshot.with_cause(&child),
                1 => snapshot.with_aggregate(std::slice::from_ref(&child)),
                2 => ExceptionSnapshot::new("Outer", &text).with_cause(&snapshot),
                _ => snapshot.with_frames(
                    &vec![
                        debugbundle::FrameSnapshot {
                            function: Some(text.to_string()),
                            ..Default::default()
                        };
                        usize::from(*byte) % 40
                    ],
                    TraceOrigin::RuntimeReported,
                ),
            };
            let mut event = base["events"][0]["event"].clone();
            event["payload"]["exception_evidence"] = snapshot.evidence().clone();
            assert!(
                debugbundle::wire::valid_event(&event),
                "step {step}: graph bytes {:?}, evidence {}",
                debugbundle::wire::canonical_size(snapshot.evidence()),
                snapshot.evidence()
            );
        }
    }
}
