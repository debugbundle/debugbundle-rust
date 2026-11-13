import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { readFile, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve, join } from "node:path";
import { createInterface } from "node:readline";

const output = resolve(".tmp/browser-qualification");
const provenance = JSON.parse(await readFile(join(output,"browser-provenance.json"),"utf8"));
const timeout = (work, ms, label) => {
  let timer;
  return Promise.race([work,new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error(`${label} timed out`)),ms);})]).finally(()=>clearTimeout(timer));
};
for (const framework of ["axum","actix"]) {
  const profile = await mkdtemp(join(tmpdir(),"rust-browser-"));
  let browser;
  const resultPath = join(output,`${framework}.json`);
  const host = spawn(join(output,"host"),[framework,join(output,"sdk.js"),resolve("smoke/browser-artifacts/fixture.html"),resultPath],{stdio:["ignore","pipe","pipe"]});
  const hostExit = once(host,"exit");
  let stderr = "";
  host.stderr.on("data",chunk=>{stderr+=chunk;if(stderr.length>65536)stderr=stderr.slice(-65536);});
  const lines = createInterface({input:host.stdout});
  try {
    const [line] = await timeout(Promise.race([once(lines,"line"),hostExit.then(()=>{throw new Error(`Host failed: ${stderr}`);})]),15000,"relay startup");
    const {origin} = JSON.parse(line);
    assert.match(origin,/^http:\/\/127\.0\.0\.1:\d+$/);
    browser = spawn(process.env.CHROMIUM_PATH,["--headless","--no-sandbox","--disable-dev-shm-usage","--disable-background-networking",`--user-data-dir=${profile}`,`${origin}/checkout?token=SYNTHETIC_BROWSER_URL_TOKEN`],{stdio:"ignore"});
    const [status] = await timeout(Promise.race([hostExit,once(browser,"error").then(([error])=>{throw error;})]),65000,"native browser relay");
    assert.equal(status,0,stderr);
    const result = JSON.parse(await readFile(resultPath,"utf8"));
    const page = result.page;
    assert.equal(page.error,undefined);
    assert.equal(page.finished,true);
    // Published Browser 3.2.2 normalizes legacy outer fields through guarded
    // native reads. Record that existing limitation independently of the new
    // evidence traversal, which must never invoke an application cause getter.
    assert(Number.isInteger(page.accessors)&&page.accessors>=0);
    assert.equal(page.causeAccessors,0,"native cause accessors never run");
    assert.equal(page.backendStatus,500);
    assert.equal(page.backendTrace,"11111111-1111-4111-8111-111111111111");
    assert.equal(page.preflightStatus,204);
    assert.equal(page.preflightCredentials,null);
    assert.equal(page.hostileStatus,202);
    assert.equal(page.hostileBody.accepted,2);
    assert.equal(page.hostileBody.rejected,0);
    assert.deepEqual(page.beacons.map(beacon=>beacon.accepted),[true,false]);
    assert.equal(page.lifecycleHookCounts.length,2);
    assert(page.lifecycleHookCounts.every(([before,after])=>before===after),"lifecycle listener must not execute deferred application hooks");
    assert(page.beacons.every(beacon=>beacon.type==="application/json"&&beacon.bytes>0&&beacon.bytes<=65536));
    assert.equal(page.exchanges.filter(exchange=>exchange.keepalive).length,1);
    assert(page.exchanges.length>=3);
    for (const exchange of page.exchanges) {
      assert.equal(exchange.authorization,null);
      assert.equal(exchange.corsCredentials,null);
      assert.equal(exchange.status,202);
      assert.equal(exchange.response.rejected,0);
      // Analytics does not request debug-evidence discovery. Its ordinary
      // canonical ACK must not be mistaken for a revoked debug proof.
      if (exchange.request.batch.some(event=>event.event_type!=="analytics_event"))
        assert(exchange.response.evidence_capabilities.features.includes("exception_evidence_v1"));
    }
    const events = result.events;
    const backend = events.find(event=>event.event_type==="backend_exception");
    assert.equal(backend.correlation.trace_id,page.backendTrace);
    assert.equal(backend.service.framework,framework==="axum"?"axum":"actix-web");
    const frontend = events.filter(event=>event.event_type==="frontend_exception");
    const analytics = events.filter(event=>event.event_type==="analytics_event");
    assert.equal(analytics.length,2,"native opted-in marker and mixed-batch replay remain analytics");
    assert(analytics.every(event=>event.schema_version==="2026-07-analytics-01"&&event.sdk_name==="@debugbundle/sdk-browser"));
    assert(frontend.length>=3);
    assert(frontend.every(event=>event.sdk_name==="@debugbundle/sdk-browser" && event.sdk_version==="3.2.2"));
    for (const message of ["Native beacon lifecycle","Native keepalive lifecycle"])
      assert.equal(frontend.filter(event=>event.payload.message===message).length,1);
    const nested = frontend.find(event=>event.payload.message.startsWith("Native outer"));
    assert.equal(nested.payload.exception_evidence.nodes.length,2);
    assert.equal(nested.context.evidence_reference.failure_ref,page.failureRef);
    const unsafe = frontend.find(event=>event.payload.message==="Native cause accessor withheld");
    assert.equal(unsafe.payload.exception_evidence.unavailable_reason,"unsafe");
    assert.equal(unsafe.payload.exception_evidence.nodes.length,1);
    const recovery = events.filter(event=>event.event_type==="diagnostic_context"&&event.payload.kind==="recovery");
    assert.deepEqual(recovery.map(event=>event.payload.state).sort(),["attempt_completed","recovery_observed"]);
    assert(recovery.every(event=>event.payload.failure_ref===page.failureRef&&event.payload.attempt_id===page.attemptId));
    const stored = JSON.stringify(events);
    for (const secret of ["PASSWORD","COOKIE","FORGED_TOKEN","ORGANIZATION","WRONG_AUTH","URL_TOKEN"]) {
      assert(!stored.includes(`SYNTHETIC_BROWSER_${secret}`),`upstream excludes ${secret}`);
    }
    assert(!stored.includes("synthetic-artifact-project-token"));
    assert(events.every(event=>event.project_token===undefined&&event.organization_id===undefined));
    console.log(`Published Browser ${provenance.browser.version} -> extracted Rust crate -> ${framework} TCP/durable relay passed (${events.length} protected upstream events; legacy outer getter reads=${page.accessors}, native cause getter reads=${page.causeAccessors})`);
  } finally {
    lines.close();
    for (const child of [browser,host]) {
      if(child&&child.exitCode===null){const exited=once(child,"exit");child.kill("SIGKILL");await exited;}
    }
    // Chromium subprocesses may finish profile writes just after the parent
    // exits. Retry removal only within this test-owned temporary directory.
    await rm(profile,{recursive:true,force:true,maxRetries:5,retryDelay:100});
  }
}
await writeFile(join(output,"acceptance.json"),JSON.stringify({browser:provenance.browser.version,frameworks:["axum","actix"],chromium:process.env.CHROMIUM_PATH,node:process.version},null,2));
