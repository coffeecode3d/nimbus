//! Process-wide stop flag and signal handling.
//!
//! Shared between the CLI (`enforce`) and the daemon (`serve`) so both shut down
//! the same way: a signal sets an atomic, and the main loop notices at its next
//! iteration and removes the policy before exiting.

use std::sync::atomic::{AtomicBool, Ordering};

static STOP: AtomicBool = AtomicBool::new(false);

/// True once SIGTERM/SIGINT/SIGHUP arrived, or stdin closed.
pub fn stopping() -> bool {
    STOP.load(Ordering::SeqCst)
}

pub fn request_stop() {
    STOP.store(true, Ordering::SeqCst);
}

extern "C" fn on_signal(_signal: libc::c_int) {
    // Only an atomic store: this runs in signal context.
    request_stop();
}

/// Install handlers for the signals nimbus-helper should clean up on.
pub fn install() {
    let handler = on_signal as *const () as libc::sighandler_t;
    // SAFETY: `on_signal` is async-signal-safe, and these are valid signals.
    unsafe {
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGHUP, handler);
    }
}

/// Rust ignores SIGPIPE by default, which turns `nimbus-helper apps | head`
/// into a panic. Restore the Unix behaviour for a CLI.
pub fn reset_sigpipe() {
    // SAFETY: SIG_DFL is a valid handler for SIGPIPE.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}
