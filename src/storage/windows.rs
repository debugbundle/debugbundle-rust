//! Pinned native ancestor handles, reparse refusal and handle-attested private
//! DACLs. This does not infer Windows privacy from readonly attributes or paths.
#[allow(unsafe_code)]
mod ffi;
use super::{denied, io, FileEntry};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Component, Path, PathBuf, Prefix},
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
};

pub(super) struct Directory {
    path: PathBuf,
    // Keeping every component open without delete-sharing prevents an ancestor
    // rename/replacement between validation and a full-path Win32 operation.
    ancestors: Vec<File>,
    identity: ffi::Identity,
}
fn directory(info: &BY_HANDLE_FILE_INFORMATION) -> bool {
    info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0
        && info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0
}
fn size(info: &BY_HANDLE_FILE_INFORMATION) -> u64 {
    (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow)
}
fn normal(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    if name.is_empty()
        || name.len() > 255
        || name.ends_with(['.', ' '])
        || name.chars().any(|c| {
            c.is_control() || matches!(c, ':' | '/' | '\\' | '<' | '>' | '"' | '|' | '?' | '*')
        })
    {
        return false;
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "CONIN$"
            | "CONOUT$"
            | "COM¹"
            | "COM²"
            | "COM³"
            | "LPT¹"
            | "LPT²"
            | "LPT³"
    ) && !(stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.as_bytes()[3].is_ascii_digit())
}
impl Directory {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        if path.as_os_str().is_empty() {
            return Err(denied());
        }
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()?.join(path)
        };
        if !path.is_absolute() || path.components().count() > 32 {
            return Err(denied());
        }
        let mut components = path.components();
        let Some(Component::Prefix(prefix)) = components.next() else {
            return Err(denied());
        };
        if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
            || components.next() != Some(Component::RootDir)
        {
            return Err(denied());
        }
        let identity = ffi::Identity::current()?;
        let mut anchor = PathBuf::from(prefix.as_os_str());
        anchor.push("\\");
        let root = ffi::open(&anchor, true, false, false, &identity)?;
        if !directory(&ffi::info(&root)?) {
            return Err(denied());
        }
        let mut ancestors = vec![root];
        for part in components {
            let name = match part {
                Component::Normal(name) if normal(name) => name,
                Component::CurDir => continue,
                _ => return Err(denied()),
            };
            anchor.push(name);
            let file = match ffi::open(&anchor, true, false, false, &identity) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    match ffi::mkdir(&anchor, &identity) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error),
                    }
                    ffi::open(&anchor, true, false, false, &identity)?
                }
                Err(error) => return Err(error),
            };
            if !directory(&ffi::info(&file)?) {
                return Err(denied());
            }
            ancestors.push(file);
        }
        let value = Self {
            path: anchor,
            ancestors,
            identity,
        };
        value.verify()?;
        Ok(value)
    }
    fn verify(&self) -> io::Result<()> {
        let handle = self.ancestors.last().ok_or_else(denied)?;
        if !directory(&ffi::info(handle)?) {
            return Err(denied());
        }
        self.identity.attest(handle)
    }
    fn regular(&self, file: &File) -> io::Result<u64> {
        let info = ffi::info(file)?;
        if info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
            || info.nNumberOfLinks != 1
            || size(&info) > crate::wire::MAX_BODY_BYTES as u64
        {
            return Err(denied());
        }
        self.identity.attest(file)?;
        Ok(size(&info))
    }
    fn child(&self, name: &str) -> io::Result<PathBuf> {
        if !normal(std::ffi::OsStr::new(name)) {
            return Err(denied());
        }
        Ok(self.path.join(name))
    }
    pub(super) fn write(&self, name: &str, body: &[u8]) -> io::Result<PathBuf> {
        self.publish(name, body, false)
    }
    fn publish(&self, name: &str, body: &[u8], replace: bool) -> io::Result<PathBuf> {
        self.verify()?;
        let destination = self.child(name)?;
        let id = crate::identity::fresh().ok_or_else(|| io::Error::other("entropy unavailable"))?;
        let temporary = self.child(&format!(".tmp-{id}"))?;
        let mut file = ffi::open(&temporary, false, true, true, &self.identity)?;
        let result = (|| {
            self.regular(&file)?;
            file.write_all(body)?;
            file.sync_all()?;
            self.regular(&file)?;
            self.verify()
        })();
        // Windows requires closing this non-delete-shared source handle before
        // its atomic no-replace/replace move. The private pinned directory stays
        // owned throughout; file data is flushed before write-through publication.
        drop(file);
        let result = result
            .and_then(|()| ffi::publish(&temporary, &destination, replace))
            .and_then(|()| self.verify());
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result.map(|()| destination)
    }
    pub(super) fn read(&self, name: &str) -> io::Result<Vec<u8>> {
        self.verify()?;
        let file = ffi::open(&self.child(name)?, false, false, false, &self.identity)?;
        self.regular(&file)?;
        let mut bytes = Vec::new();
        file.take((crate::wire::MAX_BODY_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > crate::wire::MAX_BODY_BYTES {
            return Err(denied());
        }
        self.verify()?;
        Ok(bytes)
    }
    pub(super) fn entries(&self, limit: usize) -> io::Result<Vec<FileEntry>> {
        self.verify()?;
        let mut output = Vec::new();
        for entry in std::fs::read_dir(&self.path)? {
            let entry = entry?;
            let name = entry.file_name().into_string().map_err(|_| denied())?;
            if output.len() >= limit || name.len() > 180 {
                return Err(denied());
            }
            let file = ffi::open(&self.child(&name)?, false, false, false, &self.identity)?;
            let bytes = self.regular(&file)?;
            output.push(FileEntry { name, bytes });
        }
        output.sort_by(|left, right| left.name.cmp(&right.name));
        self.verify()?;
        Ok(output)
    }
    pub(super) fn lock(&self, name: &str) -> io::Result<File> {
        self.verify()?;
        let path = self.child(name)?;
        let file = match ffi::open(&path, false, true, false, &self.identity) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match ffi::open(&path, false, true, true, &self.identity) {
                    Ok(file) => file,
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        ffi::open(&path, false, true, false, &self.identity)?
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        if self.regular(&file)? != 0 {
            return Err(denied());
        }
        file.try_lock().map_err(|_| denied())?;
        self.verify()?;
        Ok(file)
    }
    pub(super) fn replace(&self, name: &str, expected: &[u8], body: &[u8]) -> io::Result<()> {
        if self.read(name)? != expected {
            return Err(denied());
        }
        self.publish(name, body, true).map(|_| ())
    }
    pub(super) fn remove(&self, name: &str, expected: &[u8]) -> io::Result<()> {
        if self.read(name)? != expected {
            return Err(denied());
        }
        std::fs::remove_file(self.child(name)?)?;
        self.verify()
    }
}
