//! The only SDK-owned unsafe boundary. All pointers are bounded owned buffers or
//! native handles; none originates from an application error or callback. Win32
//! allocation/handle ownership is transferred once and released by RAII.
use super::super::{denied, io};
use std::{
    ffi::c_void,
    fs::File,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::Path,
    ptr,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, LocalFree, HANDLE, INVALID_HANDLE_VALUE},
    Security::{
        self,
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SE_FILE_OBJECT,
        },
    },
    Storage::FileSystem::{self as fs, BY_HANDLE_FILE_INFORMATION},
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

struct LocalMemory(*mut c_void);
impl Drop for LocalMemory {
    fn drop(&mut self) {
        // SAFETY: only successful Win32 LocalAlloc-family outputs enter this guard.
        unsafe {
            LocalFree(self.0);
        }
    }
}
struct Token(HANDLE);
impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: successful OpenProcessToken transferred one owning handle.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

pub(super) struct Identity {
    buffer: Vec<usize>,
    offset: usize,
}
impl Identity {
    pub(super) fn current() -> io::Result<Self> {
        let mut token = ptr::null_mut();
        // SAFETY: the process pseudo-handle is borrowed, output storage is valid.
        if unsafe { OpenProcessToken(GetCurrentProcess(), Security::TOKEN_QUERY, &mut token) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if token.is_null() {
            return Err(denied());
        }
        let token = Token(token);
        let mut length = 0;
        // SAFETY: the null buffer is the documented required-size query.
        unsafe {
            Security::GetTokenInformation(
                token.0,
                Security::TokenUser,
                ptr::null_mut(),
                0,
                &mut length,
            );
        }
        if !(std::mem::size_of::<Security::TOKEN_USER>()..=4096).contains(&(length as usize)) {
            return Err(denied());
        }
        let mut buffer = vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
        // SAFETY: usize storage provides TOKEN_USER alignment and at least length bytes.
        if unsafe {
            Security::GetTokenInformation(
                token.0,
                Security::TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &mut length,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if length as usize > buffer.len() * std::mem::size_of::<usize>()
            || (length as usize) < std::mem::size_of::<Security::TOKEN_USER>()
        {
            return Err(denied());
        }
        // SAFETY: success initialized the aligned TOKEN_USER prefix.
        let sid = unsafe { (*buffer.as_ptr().cast::<Security::TOKEN_USER>()).User.Sid };
        let start = buffer.as_ptr() as usize;
        let offset = (sid as usize).checked_sub(start).ok_or_else(denied)?;
        let available = (length as usize).checked_sub(offset).ok_or_else(denied)?;
        // SID header is eight bytes and at most fifteen four-byte subauthorities.
        if available < 8 {
            return Err(denied());
        }
        // SAFETY: the SID header lies within our initialized token buffer.
        let count = unsafe { *sid.cast::<u8>().add(1) } as usize;
        if count > 15 || 8 + count * 4 > available || unsafe { Security::IsValidSid(sid) } == 0 {
            return Err(denied());
        }
        Ok(Self { buffer, offset })
    }
    fn sid(&self) -> *mut c_void {
        // SAFETY: current() attests this offset and the vector never resizes.
        unsafe {
            self.buffer
                .as_ptr()
                .cast::<u8>()
                .add(self.offset)
                .cast_mut()
                .cast()
        }
    }
    fn descriptor(&self) -> io::Result<LocalMemory> {
        let mut text = ptr::null_mut();
        // SAFETY: current() validated this owned SID; output belongs to LocalFree.
        if unsafe { ConvertSidToStringSidW(self.sid(), &mut text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let memory = LocalMemory(text.cast());
        let mut length = 0;
        // SAFETY: successful SID conversion returns a terminated string; the SID
        // format is bounded, and this loop refuses any unexpectedly long output.
        while length < 184 && unsafe { *text.add(length) } != 0 {
            length += 1;
        }
        if length == 184 {
            return Err(denied());
        }
        let user = String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
            .map_err(|_| denied())?;
        drop(memory);
        let descriptor = format!("O:{user}D:P(A;OICI;FA;;;{user})(A;OICI;FA;;;SY)");
        let text = descriptor.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let mut pointer = ptr::null_mut();
        // SAFETY: terminated text and output storage live through this call.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &mut pointer,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if pointer.is_null() {
            return Err(denied());
        }
        Ok(LocalMemory(pointer))
    }
    pub(super) fn attest(&self, file: &File) -> io::Result<()> {
        let mut owner = ptr::null_mut();
        let mut dacl = ptr::null_mut();
        let mut descriptor = ptr::null_mut();
        // SAFETY: File owns a live native handle; all outputs are valid locals.
        let error = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                Security::OWNER_SECURITY_INFORMATION | Security::DACL_SECURITY_INFORMATION,
                &mut owner,
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        let _descriptor = LocalMemory(descriptor);
        if descriptor.is_null() || owner.is_null() || dacl.is_null() {
            return Err(denied());
        }
        // SAFETY: successful GetSecurityInfo points inside its owned descriptor.
        if unsafe { Security::EqualSid(owner, self.sid()) } == 0
            || unsafe { Security::IsValidAcl(dacl) } == 0
        {
            return Err(denied());
        }
        // SAFETY: IsValidAcl validated the native ACL header and its collection.
        let count = unsafe { (*dacl).AceCount };
        if count == 0 || count > 16 {
            return Err(denied());
        }
        for index in 0..count {
            let mut entry = ptr::null_mut();
            // SAFETY: index is in the validated bounded ACL; output points into it.
            if unsafe { Security::GetAce(dacl, u32::from(index), &mut entry) } == 0
                || entry.is_null()
            {
                return Err(denied());
            }
            let header = unsafe { &*entry.cast::<Security::ACE_HEADER>() };
            // ACCESS_ALLOWED_ACE is type zero. Reject every complex/conditional
            // ACE rather than approximating its security semantics.
            if header.AceType != 0 || header.AceSize < 16 {
                return Err(denied());
            }
            let allowed = entry.cast::<Security::ACCESS_ALLOWED_ACE>();
            // SAFETY: a validated simple ACE includes an eight-byte prefix/SID header.
            let sid = unsafe {
                ptr::addr_of!((*allowed).SidStart)
                    .cast_mut()
                    .cast::<c_void>()
            };
            let subauthorities = unsafe { *sid.cast::<u8>().add(1) } as usize;
            if subauthorities > 15 || 8 + subauthorities * 4 > usize::from(header.AceSize) - 8 {
                return Err(denied());
            }
            // Administrators/System are privileged native principals like Unix
            // root; no ordinary user/group may read or modify these files.
            if unsafe { Security::IsValidSid(sid) } == 0
                || !(unsafe { Security::EqualSid(sid, self.sid()) } != 0
                    || unsafe { Security::IsWellKnownSid(sid, Security::WinLocalSystemSid) } != 0
                    || unsafe {
                        Security::IsWellKnownSid(sid, Security::WinBuiltinAdministratorsSid)
                    } != 0)
            {
                return Err(denied());
            }
        }
        Ok(())
    }
}

fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let value = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if value.is_empty() || value.len() > 4096 || value.contains(&0) {
        return Err(denied());
    }
    Ok(value.into_iter().chain(Some(0)).collect())
}
pub(super) fn open(
    path: &Path,
    directory: bool,
    write: bool,
    create: bool,
    identity: &Identity,
) -> io::Result<File> {
    let path = wide(path)?;
    let descriptor = if create {
        Some(identity.descriptor()?)
    } else {
        None
    };
    let attributes = descriptor
        .as_ref()
        .map(|descriptor| Security::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<Security::SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        });
    let access = fs::FILE_GENERIC_READ | if write { fs::FILE_GENERIC_WRITE } else { 0 };
    // Deny delete-sharing for every retained ancestor and open file. Combined
    // with OPEN_REPARSE_POINT this pins the complete native path without a
    // path-check/open race or a junction redirect into another directory.
    let flags = fs::FILE_FLAG_OPEN_REPARSE_POINT
        | if directory {
            fs::FILE_FLAG_BACKUP_SEMANTICS
        } else {
            0
        };
    // SAFETY: terminated path, descriptor and SECURITY_ATTRIBUTES stay alive;
    // the returned owning handle is transferred exactly once into File.
    let handle = unsafe {
        fs::CreateFileW(
            path.as_ptr(),
            access,
            fs::FILE_SHARE_READ | fs::FILE_SHARE_WRITE,
            attributes.as_ref().map_or(ptr::null(), |value| value),
            if create {
                fs::CREATE_NEW
            } else {
                fs::OPEN_EXISTING
            },
            flags | fs::FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}
pub(super) fn mkdir(path: &Path, identity: &Identity) -> io::Result<()> {
    let path = wide(path)?;
    let descriptor = identity.descriptor()?;
    let attributes = Security::SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<Security::SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    // SAFETY: all input buffers/descriptors live through this synchronous call.
    if unsafe { fs::CreateDirectoryW(path.as_ptr(), &attributes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
pub(super) fn info(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: File owns the borrowed handle and output storage is initialized.
    if unsafe { fs::GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(information)
}
pub(super) fn publish(source: &Path, destination: &Path, replace: bool) -> io::Result<()> {
    let source = wide(source)?;
    let destination = wide(destination)?;
    // SAFETY: both terminated paths are in the separately pinned private directory.
    if unsafe {
        fs::MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            fs::MOVEFILE_WRITE_THROUGH
                | if replace {
                    fs::MOVEFILE_REPLACE_EXISTING
                } else {
                    0
                },
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
