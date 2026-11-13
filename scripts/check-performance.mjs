// Regression budgets compare an instrumented, pinned-container workload. They
// are not a capture SLA or a bound on all application/runtime allocations.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
const [reportPath, baselinePath = "smoke/performance/baselines/linux-aarch64.json"] = process.argv.slice(2);
assert(reportPath,"performance report path required");
const report = JSON.parse(await readFile(reportPath,"utf8"));
const baseline = JSON.parse(await readFile(baselinePath,"utf8"));
assert.equal(report.schema_version,1);
assert.equal(report.architecture,baseline.architecture,"this runner needs its own qualified numerical baseline");
assert.equal(report.profile,"release");
assert.equal(report.instrumentation,baseline.instrumentation);
assert(Array.isArray(report.cases));
const cases = new Map(report.cases.map(value=>[value.case,value]));
assert.equal(cases.size,report.cases.length,"duplicate performance case");
assert.equal(cases.size,baseline.cases.length,"missing or unreviewed performance case");
const caps = {
  observed_peak_event_count:1000,observed_peak_event_bytes:8*1024*1024,
  observed_peak_joint_diagnostic_bytes:65536,observed_peak_diagnostic_windows:50,
  observed_peak_operation_slots:1024,observed_peak_process_callback_workers:8,
  observed_peak_process_client_slots:8
};
for (const expected of baseline.cases) {
  const actual = cases.get(expected.case);
  assert(actual,`missing ${expected.case}`);
  assert.equal(actual.samples,expected.samples,`${expected.case}: workload changed`);
  for (const [key,value] of Object.entries(actual)) {
    if(key!=="case")assert(typeof value==="number"&&Number.isFinite(value)&&value>=0,`${expected.case}: invalid ${key}`);
  }
  assert(actual.p50_ns<=actual.p95_ns&&actual.p95_ns<=actual.p99_ns,`${expected.case}: percentile order`);
  assert(actual.p99_ns<=expected.p99_budget_ns,`${expected.case}: p99 ${actual.p99_ns} exceeds measured budget ${expected.p99_budget_ns}`);
  assert(actual.caller_allocations<=expected.caller_allocations_budget,`${expected.case}: caller allocation regression`);
  assert(actual.caller_allocated_bytes<=expected.caller_allocated_bytes_budget,`${expected.case}: caller byte regression`);
  for (const [key,cap] of Object.entries(caps)) {
    assert(Number.isInteger(actual[key])&&actual[key]<=cap,`${expected.case}: ${key} exceeds resource contract`);
  }
  if(expected.case.startsWith("pressure-"))assert.equal(actual.observed_peak_event_count,1,`${expected.case}: missing full-queue observation`);
}
console.log(`Measured performance budgets passed for ${cases.size} ${report.architecture} cases`);
