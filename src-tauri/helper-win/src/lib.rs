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
//! Layout:
//!  - `proto`     — the driver's wire format, transcribed from `defs/*.h`.
//!  - `agent`     — the decision layer, behind a `System` trait so it is testable
//!                  anywhere.
//!  - `windows_system` — the `System` implementation, all Win32 FFI.
//!  - `firewall`  — the WFP sublayers the driver's filters need, which it never
//!                  creates itself.
//!  - `driver`    — the driver client and its state machine.
//!  - `pipe`      — the named-pipe server.
//!  - `service`   — the LocalSystem service that owns the driver, plus the
//!                  request dispatch the UI talks to.
//!
//! [`mullvad/win-split-tunnel`]: https://github.com/mullvad/win-split-tunnel

pub mod agent;
pub mod proto;

#[cfg(windows)]
pub mod driver;
#[cfg(windows)]
pub mod firewall;
#[cfg(windows)]
pub mod pipe;
#[cfg(windows)]
pub mod service;
#[cfg(windows)]
pub mod windows_system;
