//! Library half of `nimbus-helper`.
//!
//! The binary is a thin CLI around these modules; Nimbus only ever talks to the
//! `serve` command over a Unix socket.

pub mod apps;
pub mod exec;
pub mod ipc;
pub mod plan;
pub mod session;
pub mod signal;
