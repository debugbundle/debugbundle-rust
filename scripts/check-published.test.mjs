import assert from "node:assert/strict";
import test from "node:test";
import { identity, registryEntry, registryConsumer } from "./check-published.mjs";

const version = "1.0.0-alpha.1", checksum = "a".repeat(64);
const entry = { name: "debugbundle", vers: version, cksum: checksum, yanked: false };
const metadata = { packages: [{ name: "debugbundle", version, source: "registry+https://github.com/rust-lang/crates.io-index" }] };
const lock = `version = 4\n[[package]]\nname = "debugbundle"\nversion = "${version}"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "${checksum}"\n`;

test("only the intended non-yanked exact registry checksum is accepted", () => {
  assert.deepEqual(registryEntry(JSON.stringify(entry), version, checksum), entry);
  for (const value of [[], [entry, entry], [{ ...entry, yanked: true }], [{ ...entry, cksum: "b".repeat(64) }], [{ ...entry, name: "other" }]])
    assert.throws(() => registryEntry(value.map(x => JSON.stringify(x)).join("\n"), version, checksum));
  for (const value of [undefined, "../1.0.0", "1.0.0\n", "1.0.0;echo bad"])
    assert.throws(() => identity(value, checksum));
  assert.throws(() => identity(version, checksum.toUpperCase()));
});

test("registry consumers reject path overrides, duplicate SDKs and different locked bytes", () => {
  registryConsumer(metadata, lock, version, checksum);
  for (const source of [null, "git+https://github.com/debugbundle/debugbundle-rust", "registry+https://other/index"])
    assert.throws(() => registryConsumer({ packages: [{ ...metadata.packages[0], source }] }, lock, version, checksum));
  assert.throws(() => registryConsumer({ packages: [...metadata.packages, ...metadata.packages] }, lock, version, checksum));
  assert.throws(() => registryConsumer(metadata, lock.replace(checksum, "b".repeat(64)), version, checksum));
  assert.throws(() => registryConsumer(metadata, lock + lock, version, checksum));
});
