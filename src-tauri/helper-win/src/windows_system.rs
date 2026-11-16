//! Windows implementation of [`crate::agent::System`].
//!
//! Three jobs:
//!  1. Turn Win32 paths into the device paths the driver matches on
//!     (`C:\Apps\x.exe` → `\Device\HarddiskVolume3\Apps\x.exe`).
//!  2. Snapshot the process tree for the driver's initial registration.
//!  3. Report which addresses belong to the tunnel and to the physical adapter.
//!
//! Everything here is unsafe FFI and is therefore kept as small and as explicit
//! as possible; the decision-making lives in `agent`, which is tested on any
//! platform.

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_BUFFER_OVERFLOW, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_INCLUDE_GATEWAYS, GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6};
use windows_sys::Win32::Storage::FileSystem::{GetLogicalDrives, QueryDosDeviceW};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};

use crate::agent::System;
use crate::proto::{IpAddresses, ProcessEntry};

/// Adapter name fragment that identifies Eddie's WireGuard adapter.
///
/// Eddie names the adapter after the tunnel; the default profile calls it
/// "Eddie". If it is ever renamed, this is the single place to change.
const TUNNEL_ADAPTER_HINT: &str = "eddie";

pub struct WindowsSystem {
    /// Drive letter → device prefix, e.g. `C` → `\Device\HarddiskVolume3`.
    drives: Vec<(char, String)>,
}

impl WindowsSystem {
    pub fn new() -> Self {
        Self {
            drives: map_drives(),
        }
    }

    /// Every running process as `(pid, win32 path)`.
    ///
    /// The picker needs the Win32 form (`C:\Program Files\...`), not the device
    /// form the driver matches on, so this deliberately bypasses
    /// `to_device_path`. Process ids repeat across images, and callers group by
    /// path.
    pub fn running_images(&self) -> Vec<(u32, String)> {
        let mut images = Vec::new();
        // SAFETY: TH32CS_SNAPPROCESS takes no additional arguments.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return images;
        }

        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

        // SAFETY: `entry` is initialised with the required size field.
        let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while more {
            let pid = entry.th32ProcessID;
            if pid > 0
                && let Some(path) = process_image(pid)
            {
                images.push((pid, path));
            }
            // SAFETY: same buffer, advanced by the API.
            more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }

        // SAFETY: the snapshot handle came from CreateToolhelp32Snapshot.
        unsafe { CloseHandle(snapshot) };
        images
    }
}

impl Default for WindowsSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl System for WindowsSystem {    fn to_device_path(&self, win32_path: &str) -> Option<String> {
        device_path(&self.drives, win32_path)
    }

    fn process_tree(&self) -> Vec<ProcessEntry> {
        process_tree(self)
    }

    fn addresses(&self) -> IpAddresses {
        adapter_addresses()
    }
}

/// Ask the kernel which device backs each drive letter.
fn map_drives() -> Vec<(char, String)> {
    let mut drives = Vec::new();
    // SAFETY: GetLogicalDrives has no preconditions and returns a bitmask.
    let mask = unsafe { GetLogicalDrives() };
    for (index, letter) in (b'A'..=b'Z').enumerate() {
        if mask & (1 << index) == 0 {
            continue;
        }
        let name = format!("{}:", letter as char);
        if let Some(target) = query_dos_device(&name) {
            drives.push((letter as char, target));
        }
    }
    drives
}

fn query_dos_device(name: &str) -> Option<String> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buffer = vec![0u16; 1024];
    // SAFETY: the input is NUL-terminated and the output buffer is sized.
    let written = unsafe {
        QueryDosDeviceW(
            wide.as_ptr(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
        )
    };
    if written == 0 {
        return None;
    }
    buffer.truncate(written as usize);
    // A drive can map to several names separated by NUL; the first is the one
    // used for image paths.
    let end = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());
    let target = String::from_utf16_lossy(&buffer[..end]);
    if target.is_empty() { None } else { Some(target) }
}

/// Convert `C:\...` to the device form, or pass through a path that is already
/// in device form. Returns `None` for UNC and unknown drives, which the caller
/// treats as "cannot be excluded" rather than guessing.
fn device_path(drives: &[(char, String)], path: &str) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Already in the form the driver wants.
    if trimmed.len() > 8 && trimmed[..8].eq_ignore_ascii_case("\\device\\") {
        return Some(trimmed.to_string());
    }
    // \\?\C:\... is the same path with the length prefix; strip it.
    let without_prefix = trimmed
        .strip_prefix("\\\\?\\")
        .filter(|rest| !rest.to_ascii_lowercase().starts_with("unc\\"))
        .unwrap_or(trimmed);
    if without_prefix.starts_with("\\\\") {
        return None; // UNC shares have no device path
    }

    let mut chars = without_prefix.chars();
    let letter = chars.next()?;
    if chars.next()? != ':' {
        return None;
    }
    let rest = chars.as_str();

    drives
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(&letter))
        .map(|(_, device)| format!("{device}{rest}"))
}

fn process_tree(system: &WindowsSystem) -> Vec<ProcessEntry> {
    // SAFETY: TH32CS_SNAPPROCESS takes no additional arguments.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Vec::new();
    }

    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

    let mut tree = Vec::new();
    // SAFETY: entry is initialised with the required size field.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while more {
        let pid = entry.th32ProcessID;
        let parent = entry.th32ParentProcessID;
        if pid > 0
            && let Some(win32) = process_image(pid)
            && let Some(image) = system.to_device_path(&win32)
        {
            tree.push(ProcessEntry {
                pid: pid as u64,
                parent_pid: parent as u64,
                image,
            });
        }
        // SAFETY: same buffer, advanced by the API.
        more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }

    // SAFETY: the snapshot handle came from CreateToolhelp32Snapshot.
    unsafe { CloseHandle(snapshot) };
    tree
}

fn process_image(pid: u32) -> Option<String> {
    // SAFETY: opening with a query-only right; no handle inheritance.
    let handle: HANDLE = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle == 0 {
        return None;
    }

    let mut buffer = vec![0u16; 32768];
    let mut size = buffer.len() as u32;
    // SAFETY: buffer and size describe the same allocation.
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size) };
    // SAFETY: the handle was opened above and is closed exactly once.
    unsafe { CloseHandle(handle) };

    if ok == 0 {
        return None;
    }
    buffer.truncate(size as usize);
    Some(String::from_utf16_lossy(&buffer))
}

/// Find the tunnel adapter and the adapter that carries the default route.
///
/// The driver needs both sides to know which binds to redirect and which to
/// block. Anything we cannot identify is left as zero, which the driver treats
/// as "that family is unavailable" — never as a guess.
fn adapter_addresses() -> IpAddresses {
    let mut addresses = IpAddresses::default();

    let mut size: u32 = 16 * 1024;
    let mut buffer = vec![0u8; size as usize];

    // SAFETY: the two-call pattern is the documented use; the buffer is
    // reallocated if the first call reports it is too small.
    let mut rc = unsafe {
        GetAdaptersAddresses(
            AF_UNSPEC as u32,
            GAA_FLAG_INCLUDE_GATEWAYS,
            std::ptr::null(),
            buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH,
            &mut size,
        )
    };
    if rc == ERROR_BUFFER_OVERFLOW {
        buffer.resize(size as usize, 0);
        // SAFETY: same call with a buffer the API said would be large enough.
        rc = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC as u32,
                GAA_FLAG_INCLUDE_GATEWAYS,
                std::ptr::null(),
                buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH,
                &mut size,
            )
        };
    }
    if rc != 0 {
        return addresses;
    }

    let mut internet_v4 = None;
    let mut internet_v6 = None;
    let mut adapter = buffer.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;

    // SAFETY: walking a linked list owned by the buffer until the null terminator.
    while let Some(current) = unsafe { adapter.as_ref() } {
        let friendly = unsafe { wide_string(current.FriendlyName) }.to_ascii_lowercase();
        let is_tunnel = friendly.contains(TUNNEL_ADAPTER_HINT);
        let has_gateway = !current.FirstGatewayAddress.is_null();

        let (v4, v6) = unsafe { unicast_addresses(current) };

        if is_tunnel {
            addresses.tunnel_v4 = addresses.tunnel_v4.or(v4);
            addresses.tunnel_v6 = addresses.tunnel_v6.or(v6);
        } else if has_gateway {
            // The adapter with a default gateway is the physical one. Prefer the
            // first, which is the lowest-metric route on a typical machine.
            internet_v4 = internet_v4.or(v4);
            internet_v6 = internet_v6.or(v6);
        }

        adapter = current.Next;
    }

    addresses.internet_v4 = internet_v4;
    addresses.internet_v6 = internet_v6;
    addresses
}

/// First IPv4 and IPv6 unicast addresses on an adapter.
unsafe fn unicast_addresses(
    adapter: &IP_ADAPTER_ADDRESSES_LH,
) -> (Option<[u8; 4]>, Option<[u8; 16]>) {
    let mut v4 = None;
    let mut v6 = None;
    let mut node = adapter.FirstUnicastAddress;

    while let Some(current) = unsafe { node.as_ref() } {
        let socket_address = current.Address;
        if !socket_address.lpSockaddr.is_null() {
            // SAFETY: the API guarantees this pointer refers to a valid sockaddr
            // of `iSockaddrLength` bytes while the adapter buffer is alive.
            let sockaddr = unsafe { &*socket_address.lpSockaddr };
            match sockaddr_family(sockaddr) {
                AF_INET if v4.is_none() => v4 = unsafe { sockaddr_v4(sockaddr) },
                AF_INET6 if v6.is_none() => v6 = unsafe { sockaddr_v6(sockaddr) },
                _ => {}
            }
        }
        node = current.Next;
    }
    (v4, v6)
}

fn sockaddr_family(addr: &SOCKADDR) -> u16 {
    addr.sa_family
}

unsafe fn sockaddr_v4(addr: &SOCKADDR) -> Option<[u8; 4]> {
    let inner = unsafe { &*(addr as *const SOCKADDR as *const SOCKADDR_IN) };
    let bytes = unsafe { inner.sin_addr.S_un.S_un_b };
    Some([bytes.s_b1, bytes.s_b2, bytes.s_b3, bytes.s_b4])
}

unsafe fn sockaddr_v6(addr: &SOCKADDR) -> Option<[u8; 16]> {
    let inner = unsafe { &*(addr as *const SOCKADDR as *const SOCKADDR_IN6) };
    Some(unsafe { inner.sin6_addr.u.Byte })
}

unsafe fn wide_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
        if len > 4096 {
            break;
        }
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    String::from_utf16_lossy(slice)
}
