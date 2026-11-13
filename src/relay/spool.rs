//! Authenticated private delivery state beside immutable public event arrays.
use crate::{storage::SecureDirectory, wire};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io,
    path::Path,
    sync::{atomic::Ordering, Arc},
};
const KEY: &str = "relay-key.v1";
const GUARD: &str = "relay.guard.v1";
const STATE: &str = ".relay.v1.json";
const DELIVERED: &str = ".delivered";
const MAX_METADATA: usize = 16 * 1024;
const MAX_FILES: usize = 1000;
const MAX_DISK: u64 = 100 * 1024 * 1024;
#[cfg(test)]
mod tests;
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Disposition {
    Pending,
    Accepted,
    Terminal,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    version: u8,
    pub file: String,
    pub scope: String,
    pub created_at: i64,
    pub completed_at: Option<i64>,
    pub next_attempt_at: i64,
    pub failures: u32,
    pub states: Vec<Disposition>,
    pub acknowledged_digests: Vec<Option<String>>,
    file_digest: String,
    proof: Vec<Value>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Signed {
    record: Record,
    mac: String,
}
pub(crate) struct Work {
    pub record: Record,
    pub metadata: Vec<u8>,
    pub events: Vec<Value>,
    pub lease: Lease,
}
pub(crate) use crate::runtime::FileReservation as LeaseReservation;
#[derive(Clone)]
pub(crate) struct LeaseControl {
    pub reservation: Arc<LeaseReservation>,
    pub slot: Arc<crate::runtime::ClientSlot>,
    pub cleanup: crate::runtime::NativeCleanup,
}
pub(crate) struct Lease {
    file: Option<File>,
    control: LeaseControl,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            self.control.cleanup.close(
                file,
                self.control.reservation.clone(),
                self.control.slot.clone(),
            );
        }
    }
}
#[derive(Default, Clone)]
pub struct SpoolStatus {
    pub ready: bool,
    pub pending_records: usize,
    pub delivered_records: usize,
    pub storage_failures: u64,
}
pub(crate) struct Scan {
    pub status: SpoolStatus,
    pub work: Option<Work>,
}
pub(crate) struct Store {
    directory: SecureDirectory,
    key: [u8; 32],
}
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
pub(crate) fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}
pub(crate) fn scope(endpoint: &str, token: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(endpoint.as_bytes());
    digest.update([0]);
    digest.update(Sha256::digest(token.as_bytes()));
    hex(&digest.finalize())
}
fn hash_text(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
fn decode(value: &str) -> Option<Vec<u8>> {
    if !hash_text(value) {
        return None;
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}
fn state_name(file: &str) -> String {
    format!("{file}{STATE}")
}
fn delivered_name(file: &str) -> String {
    format!("{file}{DELIVERED}")
}
fn lease_name(file: &str) -> String {
    format!("{file}.lease")
}
fn valid_record(record: &Record) -> bool {
    record.version == 1
        && record.file.len() <= 100
        && record.file.ends_with("-browser.events.json")
        && record
            .file
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
        && hash_text(&record.scope)
        && hash_text(&record.file_digest)
        && record.created_at >= 0
        && record
            .file
            .strip_suffix("-browser.events.json")
            .and_then(|name| name.split_once('-'))
            .is_some_and(|(time, id)| {
                time.parse::<i64>().ok() == Some(record.created_at)
                    && !id.is_empty()
                    && id.bytes().all(|byte| byte.is_ascii_digit())
                    && id.parse::<u128>().ok().is_some_and(|id| id != 0)
            })
        && record.next_attempt_at >= 0
        && record.failures <= 64
        && !record.states.is_empty()
        && record.states.len() <= 100
        && record.states.len() == record.acknowledged_digests.len()
        && record
            .states
            .iter()
            .zip(&record.acknowledged_digests)
            .all(|(state, digest)| match (state, digest) {
                (Disposition::Accepted, Some(digest)) => hash_text(digest),
                (Disposition::Pending | Disposition::Terminal, None) => true,
                _ => false,
            })
        && record.completed_at.is_none_or(|time| {
            time >= record.created_at
                && record
                    .states
                    .iter()
                    .all(|state| *state == Disposition::Accepted)
        })
        && !record.proof.is_empty()
        && record.proof.len() <= 8
        && record
            .proof
            .iter()
            .all(|proof| wire::descriptor(&wire::capabilities(proof)) == *proof)
}
impl Store {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let directory = SecureDirectory::open(path)?;
        let _guard = directory.lock(GUARD)?;
        let bytes = match directory.read(KEY) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut key = [0u8; 32];
                getrandom::fill(&mut key).map_err(|_| invalid())?;
                directory.write_named(KEY, &key)?;
                key.to_vec()
            }
            Err(error) => return Err(error),
        };
        let key = bytes.try_into().map_err(|_| invalid())?;
        Ok(Self { directory, key })
    }
    fn encode(&self, record: &Record) -> io::Result<Vec<u8>> {
        if !valid_record(record) {
            return Err(invalid());
        }
        let body = serde_json::to_vec(record).map_err(|_| invalid())?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).map_err(|_| invalid())?;
        mac.update(&body);
        let body = serde_json::to_vec(&Signed {
            record: record.clone(),
            mac: hex(&mac.finalize().into_bytes()),
        })
        .map_err(|_| invalid())?;
        if body.len() > MAX_METADATA {
            return Err(invalid());
        }
        Ok(body)
    }
    fn decode(&self, bytes: &[u8]) -> io::Result<Record> {
        if bytes.len() > MAX_METADATA {
            return Err(invalid());
        }
        let signed: Signed = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        if !valid_record(&signed.record) {
            return Err(invalid());
        }
        let body = serde_json::to_vec(&signed.record).map_err(|_| invalid())?;
        let signature = decode(&signed.mac).ok_or_else(invalid)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).map_err(|_| invalid())?;
        mac.update(&body);
        mac.verify_slice(&signature).map_err(|_| invalid())?;
        Ok(signed.record)
    }
    pub(crate) fn admit(
        &self,
        events: Vec<Value>,
        scope: &str,
        features: &[String],
        now: i64,
    ) -> io::Result<usize> {
        let _guard = self.directory.lock(GUARD)?;
        self.cleanup_locked(now)?;
        let mut parts = Vec::new();
        let mut part = Vec::new();
        let mut size = 2usize;
        for event in events {
            let valid = if event["event_type"] == "analytics_event" {
                wire::valid_analytics_event(&event)
            } else {
                wire::valid_event(&event)
            };
            if !valid {
                return Err(invalid());
            }
            let bytes = serde_json::to_vec(&event).map_err(|_| invalid())?.len();
            if bytes > wire::MAX_EVENT_BYTES {
                return Err(invalid());
            }
            let extra = bytes + usize::from(!part.is_empty());
            if extra > wire::MAX_BODY_BYTES.saturating_sub(size) {
                parts.push(part);
                part = Vec::new();
                size = 2;
            }
            size += bytes + usize::from(!part.is_empty());
            part.push(event);
        }
        if !part.is_empty() {
            parts.push(part);
        }
        if parts.is_empty() {
            return Ok(0);
        }
        let entries = self.directory.entries(MAX_FILES)?;
        if entries
            .len()
            .saturating_add(parts.len().saturating_mul(3))
            .saturating_add(2)
            > MAX_FILES
        {
            return Err(io::ErrorKind::StorageFull.into());
        }
        let occupied = entries
            .iter()
            .try_fold(0u64, |sum, entry| {
                sum.checked_add(entry.bytes.max(
                    if entry.name.ends_with(STATE) || entry.name.ends_with(DELIVERED) {
                        MAX_METADATA as u64
                    } else {
                        0
                    },
                ))
            })
            .ok_or_else(invalid)?;
        let bodies = parts
            .iter()
            .map(|part| serde_json::to_vec(part).map_err(|_| invalid()))
            .collect::<io::Result<Vec<_>>>()?;
        let added = bodies
            .iter()
            .try_fold(wire::MAX_BODY_BYTES as u64, |sum, body| {
                sum.checked_add(body.len() as u64 + MAX_METADATA as u64)
            })
            .ok_or_else(invalid)?;
        if occupied
            .checked_add(added)
            .is_none_or(|bytes| bytes > MAX_DISK)
        {
            return Err(io::ErrorKind::StorageFull.into());
        }
        let count = parts.len();
        for (part, body) in parts.into_iter().zip(bodies) {
            let id = crate::identity::fresh()
                .and_then(|id| uuid::Uuid::parse_str(&id).ok())
                .ok_or_else(invalid)?;
            let file = format!("{now}-{}-browser.events.json", id.as_u128());
            let record = Record {
                version: 1,
                file: file.clone(),
                scope: scope.into(),
                created_at: now,
                completed_at: None,
                next_attempt_at: 0,
                failures: 0,
                states: vec![Disposition::Pending; part.len()],
                acknowledged_digests: vec![None; part.len()],
                file_digest: digest(&body),
                proof: vec![wire::descriptor(features)],
            };
            let metadata = self.encode(&record)?;
            self.directory.write_named(&file, &body)?;
            let _lease = self.directory.lock(&lease_name(&file))?;
            self.directory.write_named(&state_name(&file), &metadata)?;
        }
        Ok(count)
    }
    pub(crate) fn scan(&self, scope: &str, now: i64, control: LeaseControl) -> io::Result<Scan> {
        let _guard = self.directory.lock(GUARD)?;
        self.cleanup_locked(now)?;
        let entries = self.directory.entries(MAX_FILES)?;
        let mut status = SpoolStatus {
            ready: true,
            ..SpoolStatus::default()
        };
        let mut work = None;
        for entry in entries {
            let delivered = entry.name.ends_with(DELIVERED);
            if !delivered && !entry.name.ends_with(STATE) {
                continue;
            }
            let bytes = self.directory.read(&entry.name)?;
            let record = match self.decode(&bytes) {
                Ok(record) => record,
                Err(_) => {
                    status.storage_failures = status.storage_failures.saturating_add(1);
                    status.ready = false;
                    continue;
                }
            };
            if entry.name
                != if delivered {
                    delivered_name(&record.file)
                } else {
                    state_name(&record.file)
                }
                || delivered != record.completed_at.is_some()
            {
                status.ready = false;
                status.storage_failures = status.storage_failures.saturating_add(1);
                continue;
            }
            if record.scope != scope {
                continue;
            }
            if delivered && record.completed_at.is_some() {
                status.delivered_records += 1;
                continue;
            }
            // A valid completion wins over a pending state left by a crash.
            if self
                .directory
                .read(&delivered_name(&record.file))
                .ok()
                .and_then(|bytes| self.decode(&bytes).ok())
                .is_some_and(|done| {
                    done.file == record.file
                        && done.scope == record.scope
                        && done.file_digest == record.file_digest
                        && done.completed_at.is_some()
                })
            {
                continue;
            }
            status.pending_records += 1;
            if work.is_some()
                || record.next_attempt_at > now
                || (!record.states.contains(&Disposition::Pending)
                    && record.states.contains(&Disposition::Terminal))
                || control.reservation.pending.load(Ordering::Acquire)
            {
                continue;
            }
            let lease = match self.directory.lock(&lease_name(&record.file)) {
                Ok(lease) => lease,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                Err(error) => return Err(error),
            };
            let data = self.directory.read(&record.file)?;
            if digest(&data) != record.file_digest {
                return Err(invalid());
            }
            let events: Vec<Value> = serde_json::from_slice(&data).map_err(|_| invalid())?;
            if events.len() != record.states.len()
                || events.iter().any(|event| {
                    if event["event_type"] == "analytics_event" {
                        !wire::valid_analytics_event(event)
                    } else {
                        !wire::valid_event(event)
                    }
                })
            {
                return Err(invalid());
            }
            if control
                .reservation
                .pending
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                continue;
            }
            work = Some(Work {
                record,
                metadata: bytes,
                events,
                lease: Lease {
                    file: Some(lease),
                    control: control.clone(),
                },
            });
        }
        Ok(Scan { status, work })
    }
    pub(crate) fn update(&self, record: &Record, previous: &[u8]) -> io::Result<Vec<u8>> {
        let _guard = self.directory.lock(GUARD)?;
        let old = self.decode(previous)?;
        if old.file != record.file
            || old.scope != record.scope
            || old.file_digest != record.file_digest
            || old.created_at != record.created_at
            || old.proof != record.proof
            || old.states.len() != record.states.len()
            || old.failures > record.failures
            || old.completed_at.is_some()
            || old
                .states
                .iter()
                .zip(&record.states)
                .any(|(old, new)| *old != Disposition::Pending && old != new)
            || old
                .acknowledged_digests
                .iter()
                .zip(&record.acknowledged_digests)
                .any(|(old, new)| old.is_some() && old != new)
        {
            return Err(invalid());
        }
        let body = self.encode(record)?;
        // A timed-out callback may have committed before its result was lost.
        // Retrying the exact authenticated progress is idempotent; a different
        // current record must still pass the normal compare-and-replace checks.
        if record.completed_at.is_none()
            && self
                .directory
                .read(&state_name(&record.file))
                .ok()
                .as_deref()
                == Some(body.as_slice())
        {
            return Ok(body);
        }
        if record.completed_at.is_some() {
            let name = delivered_name(&record.file);
            match self.directory.write_named(&name, &body) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let current = self.directory.read(&name)?;
                    if current != body {
                        return Err(invalid());
                    }
                }
                Err(error) => return Err(error),
            }
            match self.directory.remove(&state_name(&record.file), previous) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    // Only the exact authenticated completion proves a prior
                    // successful removal; missing state alone proves nothing.
                    if self.directory.read(&name)? != body {
                        return Err(invalid());
                    }
                }
                Err(error) => return Err(error),
            }
        } else {
            self.directory
                .replace(&state_name(&record.file), previous, &body)?;
        }
        Ok(body)
    }
    fn cleanup_locked(&self, now: i64) -> io::Result<()> {
        let entries = self.directory.entries(MAX_FILES)?;
        for entry in entries {
            let delivered = entry.name.ends_with(DELIVERED);
            if !delivered && !entry.name.ends_with(STATE) {
                continue;
            }
            let raw = match self.directory.read(&entry.name) {
                Ok(raw) => raw,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let Ok(record) = self.decode(&raw) else {
                continue;
            };
            if entry.name
                != if delivered {
                    delivered_name(&record.file)
                } else {
                    state_name(&record.file)
                }
            {
                continue;
            }
            if delivered != record.completed_at.is_some() {
                continue;
            }
            let companion_name = if delivered {
                state_name(&record.file)
            } else {
                delivered_name(&record.file)
            };
            let companion = self.directory.read(&companion_name).ok().and_then(|raw| {
                let other = self.decode(&raw).ok()?;
                (other.file == record.file
                    && other.scope == record.scope
                    && other.file_digest == record.file_digest
                    && other.created_at == record.created_at
                    && other.states.len() == record.states.len()
                    && other.completed_at.is_some() != delivered)
                    .then_some((raw, other))
            });
            let completion = record
                .completed_at
                .or_else(|| companion.as_ref().and_then(|(_, other)| other.completed_at));
            let age = now.saturating_sub(completion.unwrap_or(record.created_at));
            let ttl = if completion.is_some() {
                24 * 60 * 60 * 1000
            } else {
                7 * 24 * 60 * 60 * 1000
            };
            if age < ttl {
                continue;
            }
            let Ok(lease) = self.directory.lock(&lease_name(&record.file)) else {
                continue;
            };
            match self.directory.read(&record.file) {
                Ok(data) => {
                    if digest(&data) != record.file_digest {
                        continue;
                    }
                    self.directory.remove(&record.file, &data)?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            self.directory.remove(&entry.name, &raw)?;
            if let Some((raw, _)) = companion {
                self.directory.remove(&companion_name, &raw)?;
            }
            // All callers hold GUARD, preventing a new scanner/admission from
            // acquiring this lease between close and removal. Windows locks are
            // mandatory and the handle also denies delete-sharing.
            drop(lease);
            self.directory.remove(&lease_name(&record.file), &[])?;
        }
        Ok(())
    }
}
