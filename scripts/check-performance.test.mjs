import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

test("performance acceptance rejects missing cases, wrong runners, latency and zero-allocation regressions",async()=>{
  const directory = await mkdtemp(join(tmpdir(),"rust-performance-gate-"));
  const report = JSON.parse(await readFile(process.env.PERFORMANCE_REPORT,"utf8"));
  const baseline = JSON.parse(await readFile("smoke/performance/baselines/linux-aarch64.json","utf8"));
  const check = async value => {
    const file = join(directory,"report.json");
    await writeFile(file,JSON.stringify(value));
    return spawnSync(process.execPath,["scripts/check-performance.mjs",file],{encoding:"utf8",timeout:10000});
  };
  try {
    assert.equal((await check(report)).status,0,"actual measured report must pass before testing rejection");
    for(const change of [
      value=>value.cases.pop(),
      value=>{value.architecture="unqualified-runner";},
      value=>{value.cases[0].p99_ns=baseline.cases[0].p99_budget_ns+1;},
      value=>{value.cases.find(item=>item.case==="pressure-native-http/tracing-and-log").caller_allocations=1;},
      value=>{value.cases[0].observed_peak_joint_diagnostic_bytes=65537;},
      value=>value.cases.push(value.cases[0])
    ]) {
      const mutated = structuredClone(report);
      change(mutated);
      const result = await check(mutated);
      assert.notEqual(result.status,0,"invalid performance receipt must fail closed");
      assert.equal(result.error,undefined,"checker must exit rather than hang");
    }
  } finally { await rm(directory,{recursive:true,force:true}); }
});
