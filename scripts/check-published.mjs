// Public registry reads only. The version and checksum come from the qualified
// candidate; a successful upload alone does not establish installability.
import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";

export function identity(version, checksum) {
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version) ||
      !/^[0-9a-f]{64}$/.test(checksum)) throw new Error("Invalid release identity");
  return { version, checksum };
}

export function registryEntry(body, version, checksum) {
  identity(version, checksum);
  const entries = body.trim().split("\n").filter(Boolean).map(line => JSON.parse(line));
  const matches = entries.filter(entry => entry.name === "debugbundle" && entry.vers === version);
  if (matches.length !== 1 || matches[0].yanked !== false || matches[0].cksum !== checksum)
    throw new Error("Published version missing, ambiguous, yanked or checksum mismatch");
  return matches[0];
}

export function registryConsumer(metadata, lock, version, checksum) {
  identity(version, checksum);
  const packages = metadata.packages.filter(pkg => pkg.name === "debugbundle");
  if (packages.length !== 1 || packages[0].version !== version ||
      packages[0].source !== "registry+https://github.com/rust-lang/crates.io-index")
    throw new Error("Consumer does not resolve the intended crates.io SDK");
  const blocks = lock.split("[[package]]").filter(block => /^\s*name = "debugbundle"\s*$/m.test(block));
  if (blocks.length !== 1 || !blocks[0].includes(`version = "${version}"\n`) ||
      !blocks[0].includes('source = "registry+https://github.com/rust-lang/crates.io-index"\n') ||
      !blocks[0].includes(`checksum = "${checksum}"\n`))
    throw new Error("Registry consumer lock checksum mismatch");
}

async function bounded(url, maximum) {
  const response = await fetch(url, { redirect: "error", signal: AbortSignal.timeout(30000),
    headers: { "user-agent": "debugbundle-rust-release/1.0 (https://github.com/debugbundle/debugbundle-rust)" } });
  if (!response.ok) throw new Error(`Registry read failed: ${response.status}`);
  let size = 0;
  const chunks = [];
  for await (const chunk of response.body) {
    size += chunk.byteLength;
    if (size > maximum) throw new Error("Oversized registry response");
    chunks.push(Buffer.from(chunk));
  }
  return Buffer.concat(chunks);
}

async function main() {
  const { version, checksum } = identity(process.env.RELEASE_VERSION, process.env.RELEASE_SHA256);
  const output = new URL("../.tmp/published-registry/", import.meta.url);
  await mkdir(output, { recursive: true });
  if (process.argv[2] === "consumer") {
    registryConsumer(JSON.parse(await readFile(new URL("consumer-metadata.json", output), "utf8")),
      await readFile(new URL("consumer.lock", output), "utf8"), version, checksum);
    console.log("Registry consumer metadata and exact SDK checksum verified");
    return;
  }
  const indexUrl = "https://index.crates.io/de/bu/debugbundle";
  const index = await bounded(indexUrl, 8 * 1024 * 1024);
  const entry = registryEntry(index.toString("utf8"), version, checksum);
  const artifactUrl = `https://static.crates.io/crates/debugbundle/debugbundle-${version}.crate`;
  const artifact = await bounded(artifactUrl, 8 * 1024 * 1024);
  if (createHash("sha256").update(artifact).digest("hex") !== checksum)
    throw new Error("Downloaded crate differs from qualified candidate");
  const ownersUrl = "https://crates.io/api/v1/crates/debugbundle/owners";
  const owners = JSON.parse((await bounded(ownersUrl, 1024 * 1024)).toString("utf8"));
  if (!Array.isArray(owners.users) || owners.users.length === 0) throw new Error("No public crate owner receipt");
  await writeFile(new URL(`debugbundle-${version}.crate`, output), artifact);
  await writeFile(new URL("index.txt", output), index);
  await writeFile(new URL("receipt.json", output), JSON.stringify({ version, checksum, indexUrl, artifactUrl,
    ownersUrl, owners, entry, checked_at: new Date().toISOString() }, null, 2));
  console.log(`Published crate ${version} matches the qualified SHA-256`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) await main();
