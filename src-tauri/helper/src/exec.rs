//! Process execution.
//!
//! The helper never invokes a shell: every command is a program plus an
//! argument vector that [`crate::plan`] generated from validated values.

use std::process::{Command, Stdio};

/// Run a command and return its stdout. Non-zero exit is an error.
pub fn run(program: &str, args: &[String]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let err = err.trim();
        let detail = if err.is_empty() {
            format!("exit {}", output.status)
        } else {
            err.to_string()
        };
        return Err(format!(
            "{program} {}: {detail}",
            args.join(" ")
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Run a command, ignoring a non-zero exit. Used for idempotent cleanup, where
/// "the rule was not there" is exactly the state we want.
pub fn run_ignore(program: &str, args: &[String]) {
    let _ = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Add a firewall rule unless an identical one is already present.
///
/// `check` is the same rule with `-C` instead of `-A`. Checking first makes the
/// call idempotent, which matters because `begin` deliberately starts from a
/// clean slate but must also survive being called twice.
pub fn ensure_rule(program: &str, add: &[String], check: &[String]) -> Result<(), String> {
    if run(program, check).is_ok() {
        return Ok(());
    }
    run(program, add).map(|_| ())
}

/// Delete a firewall rule, ignoring "no such rule".
pub fn remove_rule(program: &str, delete: &[String]) {
    run_ignore(program, delete);
}

pub fn ip(args: &[String]) -> Result<String, String> {
    run("ip", args)
}
