//! Safe descriptor-relative OS calls: no path re-resolution for file operations.
use super::{denied, io};
use rustix::fs::{self, AtFlags, Mode, OFlags, RenameFlags};
use std::{
    fs::{File, Metadata},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};
pub(super) struct Directory {
    path: PathBuf,
    handle: File,
}
fn flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
fn walk(path: &Path, create: bool) -> io::Result<File> {
    let mut parent = File::open("/")?;
    for part in path.components() {
        let name = match part {
            Component::Normal(name) => name,
            Component::RootDir | Component::CurDir => continue,
            _ => return Err(denied()),
        };
        let opened = match fs::openat(&parent, name, flags(), Mode::empty()) {
            Ok(opened) => opened,
            Err(rustix::io::Errno::NOENT) if create => {
                match fs::mkdirat(&parent, name, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                    Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => return Err(error.into()),
                }
                fs::openat(&parent, name, flags(), Mode::empty())?
            }
            Err(error) => return Err(error.into()),
        };
        parent = File::from(opened);
    }
    Ok(parent)
}
fn private(metadata: &Metadata) -> bool {
    metadata.uid() == rustix::process::geteuid().as_raw()
        && metadata.permissions().mode() & 0o077 == 0
}
fn regular(metadata: &Metadata) -> bool {
    metadata.is_file()
        && metadata.nlink() == 1
        && private(metadata)
        && metadata.len() <= crate::wire::MAX_BODY_BYTES as u64
}
impl Directory {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        if path.as_os_str().is_empty()
            || path.as_os_str().len() > 4096
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(denied());
        }
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()?.join(path)
        };
        if path.components().count() > 32 {
            return Err(denied());
        }
        let handle = walk(&path, true)?;
        let metadata = handle.metadata()?;
        if !metadata.is_dir() || !private(&metadata) {
            return Err(denied());
        }
        Ok(Self { path, handle })
    }
    fn verify(&self) -> io::Result<()> {
        let current = walk(&self.path, false)?.metadata()?;
        let owned = self.handle.metadata()?;
        if !current.is_dir()
            || !private(&current)
            || (current.dev(), current.ino()) != (owned.dev(), owned.ino())
        {
            return Err(denied());
        }
        Ok(())
    }
    pub(super) fn write(&self, name: &str, body: &[u8]) -> io::Result<PathBuf> {
        self.publish(name, body, false)
    }
    fn publish(&self, name: &str, body: &[u8], replace: bool) -> io::Result<PathBuf> {
        self.verify()?;
        let identity =
            crate::identity::fresh().ok_or_else(|| io::Error::other("entropy unavailable"))?;
        let temporary = format!(".tmp-{identity}");
        let mut file = File::from(fs::openat(
            &self.handle,
            temporary.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?);
        let result = (|| {
            file.write_all(body)?;
            file.sync_all()?;
            if !regular(&file.metadata()?) {
                return Err(denied());
            }
            self.verify()?;
            // Kernel-enforced no-replace publication closes the check/rename
            // race between processes choosing the same event filename.
            fs::renameat_with(
                &self.handle,
                temporary.as_str(),
                &self.handle,
                name,
                if replace {
                    RenameFlags::empty()
                } else {
                    RenameFlags::NOREPLACE
                },
            )?;
            self.handle.sync_all()?;
            self.verify()?;
            Ok(self.path.join(name))
        })();
        if result.is_err() {
            let _ = fs::unlinkat(&self.handle, temporary.as_str(), AtFlags::empty());
        }
        result
    }
    pub(super) fn read(&self, name: &str) -> io::Result<Vec<u8>> {
        self.verify()?;
        // NONBLOCK avoids waiting if a competing local process substitutes a
        // FIFO between metadata checks; descriptor metadata must prove regular.
        let file = File::from(fs::openat(
            &self.handle,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        if !regular(&file.metadata()?) {
            return Err(denied());
        }
        let mut bytes = Vec::new();
        file.take((crate::wire::MAX_BODY_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > crate::wire::MAX_BODY_BYTES {
            return Err(denied());
        }
        self.verify()?;
        Ok(bytes)
    }
    pub(super) fn entries(&self, limit: usize) -> io::Result<Vec<super::FileEntry>> {
        self.verify()?;
        let entries = fs::Dir::read_from(&self.handle)?;
        let mut output = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_str().map_err(|_| denied())?;
            if name == "." || name == ".." {
                continue;
            }
            if output.len() >= limit || name.len() > 180 {
                return Err(denied());
            }
            let metadata = fs::statat(&self.handle, name, AtFlags::SYMLINK_NOFOLLOW)?;
            if fs::FileType::from_raw_mode(metadata.st_mode) != fs::FileType::RegularFile
                || metadata.st_nlink != 1
                || metadata.st_uid != rustix::process::geteuid().as_raw()
                || metadata.st_mode & 0o077 != 0
            {
                return Err(denied());
            }
            let bytes = u64::try_from(metadata.st_size).map_err(|_| denied())?;
            if bytes > crate::wire::MAX_BODY_BYTES as u64 {
                return Err(denied());
            }
            output.push(super::FileEntry {
                name: name.to_owned(),
                bytes,
            });
        }
        self.verify()?;
        output.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(output)
    }
    pub(super) fn lock(&self, name: &str) -> io::Result<File> {
        self.verify()?;
        let file = File::from(fs::openat(
            &self.handle,
            name,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?);
        let metadata = file.metadata()?;
        if !regular(&metadata) || metadata.len() != 0 {
            return Err(denied());
        }
        fs::flock(&file, fs::FlockOperation::NonBlockingLockExclusive)?;
        self.verify()?;
        Ok(file)
    }
    pub(super) fn replace(&self, name: &str, expected: &[u8], body: &[u8]) -> io::Result<()> {
        // Callers hold the stable spool lock and authenticate the old metadata.
        // This comparison fences an unexpected version; it is not authentication.
        if self.read(name)? != expected {
            return Err(denied());
        }
        self.publish(name, body, true).map(|_| ())
    }
    pub(super) fn remove(&self, name: &str, expected: &[u8]) -> io::Result<()> {
        if self.read(name)? != expected {
            return Err(denied());
        }
        fs::unlinkat(&self.handle, name, AtFlags::empty())?;
        self.handle.sync_all()?;
        self.verify()
    }
}
