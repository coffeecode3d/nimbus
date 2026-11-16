//! A very small named-pipe server.
//!
//! The Linux helper accepts one JSON request per connection on a Unix socket;
//! this is the Windows equivalent, so the client side (and the protocol) stay
//! the same shape. Each connection carries exactly one line in and one line
//! out, which keeps the driver's single exclusive handle free of long-lived
//! clients.
//!
//! Pipes here are byte mode and message framing is the client's newline, which
//! is what `serde_json` + `read_line` already do on the Linux side.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::{
    FlushFileBuffers, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

/// SYSTEM, Administrators and the interactive user.
///
/// Administrators alone is not enough: the desktop app runs with a UAC-filtered
/// token, which cannot use a DACL that grants the Administrators group, so the
/// feature would report "service not installed" in the normal case. Verified in
/// the VM: an unelevated PowerShell gets `Access is denied` on a
/// SYSTEM+Administrators DACL.
///
/// The trade-off is that any locally logged-in user can change the exclude list,
/// which is machine-wide: they could push an application out of the tunnel for
/// everyone on the machine. That only ever reduces protection for the listed
/// images and is visible in the UI, and Mullvad's own client takes a similar
/// view. Tightening this means impersonating the caller and scoping the policy
/// to a session, which the driver does not currently support.
const SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;IU)";

/// `ConnectNamedPipe` reports this when the client connected before the call.
const ERROR_PIPE_CONNECTED: u32 = 535;

pub struct PipeServer {
    name: Vec<u16>,
    /// SYSTEM and Administrators only, expressed as SDDL so the intent is
    /// explicit rather than inherited from whatever default DACL the service
    /// token happens to carry.
    attributes: SECURITY_ATTRIBUTES,
    descriptor: PSECURITY_DESCRIPTOR,
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            // SAFETY: the descriptor came from the conversion above and is freed
            // exactly once.
            unsafe { LocalFree(self.descriptor as *mut core::ffi::c_void) };
        }
    }
}

impl PipeServer {
    pub fn new(name: &str) -> Self {
        let sddl = wide(SDDL);
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: the SDDL is NUL-terminated and the out pointer is valid.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1, // SDDL_REVISION_1
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        let descriptor = if ok == 0 {
            std::ptr::null_mut()
        } else {
            descriptor
        };

        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };

        Self {
            name: wide(name),
            attributes,
            descriptor,
        }
    }

    /// Block until a client connects, then hand back the connection.
    ///
    /// Returns `None` if the instance could not be created or a client failed to
    /// connect; the caller just tries again.
    pub fn accept(&self) -> Option<PipeConnection> {
        // SAFETY: the name is NUL-terminated and lives as long as `self`; the
        // security descriptor is owned by `self` too.
        let handle = unsafe {
            CreateNamedPipeW(
                self.name.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                &self.attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }

        // SAFETY: `handle` is a pipe instance created above.
        let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) } != 0;
        // SAFETY: GetLastError has no preconditions.
        let already = unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        if !connected && !already {
            // SAFETY: the instance was created above and is not in use.
            unsafe { CloseHandle(handle) };
            return None;
        }

        Some(PipeConnection { handle })
    }
}

pub struct PipeConnection {
    handle: HANDLE,
}

impl PipeConnection {
    /// Read one newline-terminated request. `None` means the client went away.
    pub fn read_line(&self) -> Option<String> {
        let mut out = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let mut read: u32 = 0;
            // SAFETY: the buffer is valid for its length and `read` is written by
            // the call.
            let ok = unsafe {
                ReadFile(
                    self.handle,
                    buffer.as_mut_ptr(),
                    buffer.len() as u32,
                    &mut read,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 || read == 0 {
                return None;
            }
            let chunk = &buffer[..read as usize];
            if let Some(newline) = chunk.iter().position(|&byte| byte == b'\n') {
                out.extend_from_slice(&chunk[..newline]);
                return String::from_utf8(out).ok();
            }
            out.extend_from_slice(chunk);
            if out.len() > 1 << 20 {
                return None; // a request this large is not one of ours
            }
        }
    }

    pub fn write_all(&self, bytes: &[u8]) -> bool {
        let mut written: u32 = 0;
        let mut offset = 0usize;
        while offset < bytes.len() {
            let mut wrote: u32 = 0;
            // SAFETY: the slice from `offset` is valid for the length given.
            let ok = unsafe {
                WriteFile(
                    self.handle,
                    bytes[offset..].as_ptr(),
                    (bytes.len() - offset) as u32,
                    &mut wrote,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return false;
            }
            written += wrote;
            offset += wrote as usize;
            if wrote == 0 {
                return false;
            }
        }
        // SAFETY: the handle came from CreateNamedPipeW.
        unsafe { FlushFileBuffers(self.handle) };
        written as usize == bytes.len()
    }
}

impl Drop for PipeConnection {
    fn drop(&mut self) {
        // SAFETY: this connection owns the handle; disconnect then close once.
        unsafe {
            DisconnectNamedPipe(self.handle);
            CloseHandle(self.handle);
        }
    }
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(std::iter::once(0)).collect()
}
