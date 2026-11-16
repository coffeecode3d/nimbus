//! Thin client for `mullvad/win-split-tunnel`, plus a console harness.
//!
//! The driver exposes one device and a handful of IOCTLs. This module opens it,
//! walks the state machine the driver documents, and drains its event queue.
//!
//! # State machine
//!
//! ```text
//! STARTED      -- IOCTL_ST_INITIALIZE (sublayer GUIDs) -->
//! INITIALIZED  -- IOCTL_ST_REGISTER_PROCESSES (full tree) -->
//! READY        -- IOCTL_ST_SET_CONFIGURATION + IOCTL_ST_REGISTER_IP_ADDRESSES -->
//! ENGAGED      -- IOCTL_ST_CLEAR_CONFIGURATION -->
//! READY
//! ```
//!
//! Everything here requires administrator rights: the device object is created
//! for SYSTEM. Nimbus itself stays unprivileged; the eventual home for this code
//! is a small Windows service installed by the NSIS installer.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

use crate::proto;

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(std::iter::once(0)).collect()
}

pub struct Driver {
    handle: HANDLE,
}

impl Driver {
    /// Open the driver's device object.
    pub fn open() -> Result<Self, String> {
        let path = wide(proto::DEVICE_PATH);
        // SAFETY: `path` is NUL-terminated and outlives the call.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                FILE_GENERIC_READ | FILE_GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                0, // htemplatefile: HANDLE is an isize in windows-sys 0.52
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            // SAFETY: GetLastError has no preconditions.
            let code = unsafe { GetLastError() };
            return Err(format!(
                "could not open {}: error {code}. Is the Mullvad split tunnel driver \
                 installed, and is this process elevated?",
                proto::DEVICE_PATH
            ));
        }
        Ok(Self { handle })
    }

    fn ioctl_raw(
        &self,
        code: u32,
        input: Option<&[u8]>,
        output_len: usize,
    ) -> Result<Vec<u8>, String> {
        let mut output = vec![0u8; output_len];
        let mut returned: u32 = 0;
        let (in_ptr, in_len) = match input {
            Some(bytes) => (bytes.as_ptr() as *const core::ffi::c_void, bytes.len() as u32),
            None => (std::ptr::null(), 0),
        };
        // SAFETY: input points at `input` for `in_len` bytes, output at `output`
        // for `output_len` bytes, and both live for the duration of the call.
        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                code,
                in_ptr,
                in_len,
                output.as_mut_ptr() as *mut core::ffi::c_void,
                output.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            // SAFETY: GetLastError has no preconditions.
            let error = unsafe { GetLastError() };
            return Err(format!(
                "driver IOCTL 0x{code:08X} failed (Win32 error 0x{error:08X})"
            ));
        }
        output.truncate(returned as usize);
        Ok(output)
    }

    fn ioctl(&self, code: u32, input: &[u8]) -> Result<(), String> {
        self.ioctl_raw(code, Some(input), 0).map(|_| ())
    }

    /// STARTED → INITIALIZED.
    pub fn initialize(&self, sublayers: &proto::SublayerGuids) -> Result<(), String> {
        self.ioctl(proto::IOCTL_ST_INITIALIZE, &sublayers.encode())
    }

    /// Return the driver to `STARTED`, releasing everything a previous client
    /// registered.
    ///
    /// The driver keeps its state across clients: a process that engaged it and
    /// then exited leaves it in `READY` or `ENGAGED`, where `INITIALIZE` is no
    /// longer valid and the first IOCTL fails with `STATUS_INVALID_DEVICE_REQUEST`
    /// (Win32 `ERROR_INVALID_FUNCTION`). Reset before initialising.
    pub fn reset(&self) -> Result<(), String> {
        self.ioctl_raw(proto::IOCTL_ST_RESET, None, 0).map(|_| ())
    }

    /// INITIALIZED → READY: hand over the process tree that already exists.
    pub fn register_processes(&self, tree: &[proto::ProcessEntry]) -> Result<(), String> {
        self.ioctl(proto::IOCTL_ST_REGISTER_PROCESSES, &proto::processes(tree))
    }

    /// READY → ENGAGED: which addresses belong to the tunnel and to the physical
    /// adapter. Zero addresses mean "this family is unavailable".
    pub fn register_addresses(&self, addresses: proto::IpAddresses) -> Result<(), String> {
        self.ioctl(
            proto::IOCTL_ST_REGISTER_IP_ADDRESSES,
            &addresses.encode(),
        )
    }

    /// Which images to exclude. Empty means nothing is excluded.
    pub fn set_configuration(&self, images: &[String]) -> Result<(), String> {
        self.ioctl(
            proto::IOCTL_ST_SET_CONFIGURATION,
            &proto::configuration(images),
        )
    }

    /// ENGAGED → READY.
    pub fn clear_configuration(&self) -> Result<(), String> {
        self.ioctl(proto::IOCTL_ST_CLEAR_CONFIGURATION, &[])
    }

    pub fn state(&self) -> Result<proto::DriverState, String> {
        let out = self.ioctl_raw(proto::IOCTL_ST_GET_STATE, None, 8)?;
        let raw = out
            .get(0..4)
            .and_then(|b| b.try_into().ok())
            .map(u32::from_le_bytes)
            .unwrap_or(0);
        Ok(proto::DriverState::from_raw(raw))
    }

    /// Drain one event. `Ok(None)` means the queue is empty.
    pub fn next_event(&self) -> Result<Option<proto::Event>, String> {
        match self.ioctl_raw(proto::IOCTL_ST_DEQUEUE_EVENT, None, 8192) {
            Ok(out) if out.is_empty() => Ok(None),
            Ok(out) => Ok(proto::parse_event(&out)),
            // A blocking dequeue that is cancelled reports an error; treat it as
            // "nothing right now" rather than a failure.
            Err(_) => Ok(None),
        }
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        if self.handle != INVALID_HANDLE_VALUE {
            // SAFETY: the handle came from CreateFileW and is closed once.
            unsafe { CloseHandle(self.handle) };
        }
    }
}

/// Everything needed to engage the driver for one session.
pub struct Session {
    pub images: Vec<String>,
    pub addresses: proto::IpAddresses,
    pub processes: Vec<proto::ProcessEntry>,
}

/// Console entry point: engage with a JSON session description and print events.
///
/// This exists so the integration can be exercised on a Windows machine before
/// the service wrapper exists:
///
/// ```text
/// nimbus-split-win --session session.json
/// ```
///
/// with `{"images": ["\\Device\\...\\helium.exe"],
///        "tunnel_v4": "10.8.0.2", "internet_v4": "192.168.1.10"}`
pub fn run_console_session(path: &str) -> Result<(), String> {
    use crate::agent::System;

    #[derive(serde::Deserialize)]
    struct SessionFile {
        /// Win32 executable paths to exclude, e.g. `C:\\Program Files\\x\\x.exe`.
        apps: Vec<String>,
        /// Optional overrides, so the integration can be tested without a tunnel.
        #[serde(default)]
        tunnel_v4: Option<String>,
        #[serde(default)]
        internet_v4: Option<String>,
    }

    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read {path}: {e}"))?;
    let file: SessionFile =
        serde_json::from_str(&text).map_err(|e| format!("could not parse {path}: {e}"))?;

    let system = crate::windows_system::WindowsSystem::new();
    let mut plan = crate::agent::plan(&system, &file.apps);
    // Overrides exist for testing the driver without a live tunnel.
    let parse_v4 = |value: &Option<String>| -> Option<[u8; 4]> {
        let ip: std::net::Ipv4Addr = value.as_deref()?.parse().ok()?;
        Some(ip.octets())
    };
    if let Some(v4) = parse_v4(&file.tunnel_v4) {
        plan.addresses.tunnel_v4 = Some(v4);
    }
    if let Some(v4) = parse_v4(&file.internet_v4) {
        plan.addresses.internet_v4 = Some(v4);
    }

    println!(
        "plan: {} image(s), {} process(es), tunnel {:?}, internet {:?}",
        plan.images.len(),
        plan.tree.len(),
        plan.addresses.tunnel_v4.map(std::net::Ipv4Addr::from),
        plan.addresses.internet_v4.map(std::net::Ipv4Addr::from),
    );
    for image in &plan.images {
        println!("  exclude {image}");
    }
    if !crate::agent::is_meaningful(&plan) {
        eprintln!("warning: nothing matched, the driver would engage with an empty list");
    }
    let _ = system.addresses();

    let driver = Driver::open()?;
    let state = driver.state()?;
    println!("driver state: {state:?}");

    // A previous client may have left the driver mid-sequence, where INITIALIZE
    // is rejected. Reset first so the sequence always starts from STARTED.
    if state != proto::DriverState::Started {
        println!("resetting the driver to STARTED");
        driver.reset()?;
        println!("driver state after reset: {:?}", driver.state()?);
    }

    // Two stable GUIDs of our own; the driver only needs them to namespace its
    // WFP filters. They must stay the same across restarts so stale filters can
    // be found and removed.
    let sublayers = proto::SublayerGuids {
        baseline: *b"nimbus-split-bas",
        dns: *b"nimbus-split-dns",
    };
    // The driver attaches its filters to two sublayers that it never creates
    // itself; registering them first is what makes engaging possible.
    let firewall = crate::firewall::Firewall::open()?;
    firewall.ensure_sublayer(sublayers.baseline, "Nimbus split tunnel")?;
    firewall.ensure_sublayer(sublayers.dns, "Nimbus split tunnel DNS")?;
    println!("WFP sublayers registered");

    driver.initialize(&sublayers)?;
    driver.register_processes(&plan.tree)?;
    driver.register_addresses(plan.addresses)?;
    driver.set_configuration(&plan.images)?;

    // The driver only enters ENGAGED when it has a tunnel address to steer
    // towards; with no tunnel it accepts the configuration and waits in READY.
    // Report what it actually says rather than assuming.
    match driver.state()? {
        proto::DriverState::Engaged => println!(
            "engaged; excluding {} image(s)",
            plan.images.len()
        ),
        other => println!(
            "configuration accepted; driver is {other:?} (no tunnel address, so nothing is \
             steered yet); {} image(s) staged",
            plan.images.len()
        ),
    }

    loop {
        match driver.next_event()? {
            Some(event) => println!("{event:?}"),
            None => std::thread::sleep(std::time::Duration::from_millis(500)),
        }
    }
}
