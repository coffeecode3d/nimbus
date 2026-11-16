//! Windows exclude-mode split tunneling for Nimbus.
//!
//! Per-application routing on Windows needs a kernel driver, and Windows only
//! loads drivers signed by Microsoft. Rather than buy a certificate and maintain
//! a driver, Nimbus drives [`mullvad/win-split-tunnel`], which is GPL-3.0 and
//! already signed.
//!
//! Semantics: the driver implements **exclude** mode — listed applications are
//! pushed out of the tunnel while everything else stays in. Eddie runs in its
//! normal `in` mode.
//!
//! [`mullvad/win-split-tunnel`]: https://github.com/mullvad/win-split-tunnel

pub mod agent;
pub mod proto;

#[cfg(windows)]
pub mod driver;
#[cfg(windows)]
pub mod firewall;
#[cfg(windows)]
pub mod windows_system;
