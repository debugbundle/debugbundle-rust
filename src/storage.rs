//! Private owned file storage, never a fallback to HTTP.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix;
#[cfg(windows)]
mod windows;
use serde_json::Value;
use std::{
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use unix as platform;
#[cfg(windows)]
use windows as platform;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
pub struct SecureDirectory {
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    inner: platform::Directory,
}
pub(crate) struct FileEntry {
    pub name: String,
    pub bytes: u64,
}
fn denied() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
/// Filename labels are independent of canonical service identity, including
/// valid owner-hook replacements. Never interpret a service as a path.
pub(crate) fn filename_service(service: &str) -> String {
    let label: String = service
        .chars()
        .take(64)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '-'
            }
        })
        .collect();
    if label.is_empty() {
        "rust-service".into()
    } else {
        label
    }
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn unsupported<T>() -> io::Result<T> {
    Err(io::ErrorKind::Unsupported.into())
}
impl SecureDirectory {
    pub fn open(path: &Path) -> io::Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        {
            platform::Directory::open(path).map(|inner| Self { inner })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            let _ = path;
            unsupported()
        }
    }
    pub fn write_events(&self, events: &[Value], service: &str) -> io::Result<PathBuf> {
        if events.is_empty()
            || events.len() > 100
            || service.is_empty()
            || service.len() > 64
            || !service
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err(denied());
        }
        let privacy = crate::privacy::Privacy::new(&[]).ok_or_else(denied)?;
        let protected = events
            .iter()
            .map(|event| {
                if event["event_type"] == "analytics_event" {
                    crate::analytics_wire::protect_event(&privacy, event)
                } else {
                    crate::wire::protect_event(&privacy, event)
                }
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(denied)?;
        let body = serde_json::to_vec(&protected).map_err(|_| denied())?;
        let name = format!(
            "{}-{}-{service}.events.json",
            chrono::Utc::now().timestamp_millis(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        self.write_named(&name, &body)
    }
    pub(crate) fn write_named(&self, name: &str, body: &[u8]) -> io::Result<PathBuf> {
        if !safe_name(name) || body.len() > crate::wire::MAX_BODY_BYTES {
            return Err(denied());
        }
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        {
            self.inner.write(name, body)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        unsupported()
    }
    pub fn read(&self, name: &str) -> io::Result<Vec<u8>> {
        if !safe_name(name) {
            return Err(denied());
        }
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        {
            self.inner.read(name)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        unsupported()
    }
    pub(crate) fn entries(&self, limit: usize) -> io::Result<Vec<FileEntry>> {
        if limit > 1000 {
            return Err(denied());
        }
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        {
            self.inner.entries(limit)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        unsupported()
    }
    pub(crate) fn lock(&self, name: &str) -> io::Result<std::fs::File> {
        if !safe_name(name) {
            return Err(denied());
        }
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        {
            self.inner.lock(name)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        unsupported()
    }
    pub(crate) fn replace(&self, name: &str, expected: &[u8], body: &[u8]) -> io::Result<()> {
        if !safe_name(name) || body.len() > crate::wire::MAX_BODY_BYTES {
            return Err(denied());
        }
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        {
            self.inner.replace(name, expected, body)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            let _ = expected;
            unsupported()
        }
    }
    pub(crate) fn remove(&self, name: &str, expected: &[u8]) -> io::Result<()> {
        if !safe_name(name) {
            return Err(denied());
        }
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        {
            self.inner.remove(name, expected)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            let _ = expected;
            unsupported()
        }
    }
}
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 180
        && !name.starts_with('.')
        && !name.contains("..")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

#[cfg(test)]
mod tests;
