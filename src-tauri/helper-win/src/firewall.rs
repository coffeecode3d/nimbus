//! The WFP sublayers the split tunnel driver expects to already exist.
//!
//! `mullvad/win-split-tunnel` creates its own WFP *provider*, callouts and
//! filters, but it never creates the two *sublayers* those filters are attached
//! to. It reads their GUIDs from `IOCTL_ST_INITIALIZE` and assumes the
//! client registered them, which is what Mullvad's own daemon does.
//!
//! Without this, engaging fails at `IOCTL_ST_SET_CONFIGURATION` with
//! `FWP_E_SUBLAYER_NOT_FOUND` (`0x80320007`) — which reads like "already
//! exists" in some error tables, so it is worth naming precisely. Registering a
//! sublayer that a previous run left behind reports `FWP_E_ALREADY_EXISTS`
//! (`0x80320009`) and is treated as success.
//!
//! The GUIDs are ours: any pair works, provided the ones registered here are the
//! same ones handed to the driver, and they stay stable across restarts so a
//! previous run's sublayers can be found and reused.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::NetworkManagement::WindowsFilteringPlatform::{
    FWPM_DISPLAY_DATA0, FWPM_SESSION0, FWPM_SUBLAYER0, FwpmEngineClose0, FwpmEngineOpen0,
    FwpmSubLayerAdd0,
};
use windows_sys::Win32::System::Rpc::RPC_C_AUTHN_WINNT;

/// The sublayer is already registered, which is success for our purposes.
const FWP_E_ALREADY_EXISTS: u32 = 0x8032_0009;

/// An open WFP engine session, closed on drop.
pub struct Firewall {
    engine: HANDLE,
}

impl Firewall {
    /// Open a session to the local filtering engine. Needs administrator rights.
    pub fn open() -> Result<Self, String> {
        let mut engine: HANDLE = 0;
        // SAFETY: a null server name means the local engine, and a null identity
        // is required with RPC_C_AUTHN_WINNT. `session` is a zeroed struct, which
        // is the documented way to ask for the default session.
        let session: FWPM_SESSION0 = unsafe { std::mem::zeroed() };
        let status = unsafe {
            FwpmEngineOpen0(
                std::ptr::null(),
                RPC_C_AUTHN_WINNT as u32,
                std::ptr::null(),
                &session,
                &mut engine,
            )
        };
        if status != 0 {
            return Err(format!(
                "could not open the WFP engine (0x{status:08X}); this needs administrator rights"
            ));
        }
        Ok(Self { engine })
    }

    /// Register one sublayer, tolerating the case where it is already there.
    pub fn ensure_sublayer(&self, guid: [u8; 16], name: &str) -> Result<(), String> {
        let mut display_name = wide(name);
        let key = guid_from_bytes(guid);

        let sublayer = FWPM_SUBLAYER0 {
            subLayerKey: key,
            displayData: FWPM_DISPLAY_DATA0 {
                name: display_name.as_mut_ptr(),
                description: std::ptr::null_mut(),
            },
            flags: 0,
            // No provider: these sublayers belong to us, not to the driver's
            // provider, which is what lets the driver's filters reference them.
            providerKey: std::ptr::null_mut(),
            providerData: unsafe { std::mem::zeroed() },
            // Weight only orders sublayers against each other; any non-zero
            // value is fine and it is not a filter priority.
            weight: 0x8000,
        };

        // SAFETY: the sublayer and its display name outlive the call, and a null
        // security descriptor means the default.
        let status =
            unsafe { FwpmSubLayerAdd0(self.engine, &sublayer, std::ptr::null_mut()) };
        if status == 0 || status == FWP_E_ALREADY_EXISTS {
            return Ok(());
        }
        Err(format!("could not register WFP sublayer {name} (0x{status:08X})"))
    }
}

impl Drop for Firewall {
    fn drop(&mut self) {
        // SAFETY: the handle came from FwpmEngineOpen0 and is closed once.
        unsafe { FwpmEngineClose0(self.engine) };
    }
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(std::iter::once(0)).collect()
}

/// Reinterpret 16 opaque bytes as a GUID.
///
/// The driver only ever compares GUIDs for equality, so the internal field
/// layout does not matter as long as it is applied consistently.
fn guid_from_bytes(bytes: [u8; 16]) -> windows_sys::core::GUID {
    windows_sys::core::GUID {
        data1: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        data2: u16::from_le_bytes([bytes[4], bytes[5]]),
        data3: u16::from_le_bytes([bytes[6], bytes[7]]),
        data4: [
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ],
    }
}
