//! Client for the `nimbus-helper` daemon.
//!
//! Nimbus never runs privileged code itself. A root systemd service owns the
//! cgroup, firewall mark, and policy route, and this module asks it over a Unix
//! socket. The service decides what the caller may do from `SO_PEERCRED`, so
//! nothing the frontend sends can reach another user's processes.
//!
//! If the service is not installed the calls fail with a message the UI can
//! show directly; app split tunneling simply stays unavailable.

use serde_json::Value;
use std::time::Duration;

#[cfg(target_os = "linux")]
const SOCKET: &str = "/run/nimbus/helper.sock";

/// Where the Windows service serves the same protocol.
#[cfg(target_os = "windows")]
const PIPE: &str = r"\\.\pipe\nimbus-split";

/// One JSON request, one JSON response. Blocks, so callers use
/// `spawn_blocking`.
#[cfg(target_os = "linux")]
fn call(request: Value) -> Result<Value, String> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(SOCKET).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            "App split tunneling needs the Nimbus helper service, which is not installed."
                .to_string()
        }
        std::io::ErrorKind::PermissionDenied => {
            "The Nimbus helper refused the connection.".to_string()
        }
        _ => format!("Could not reach the Nimbus helper at {SOCKET}: {e}"),
    })?;

    // `split_begin` installs routes and moves processes, so allow it a moment,
    // but never hang the UI indefinitely.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let mut payload = serde_json::to_string(&request)
        .map_err(|e| format!("Could not encode the helper request: {e}"))?;
    payload.push('\n');
    stream
        .write_all(payload.as_bytes())
        .map_err(|e| format!("Could not send the helper request: {e}"))?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("The Nimbus helper did not answer: {e}"))?;
    if line.trim().is_empty() {
        return Err("The Nimbus helper closed the connection without answering.".into());
    }

    let response: Value = serde_json::from_str(&line)
        .map_err(|e| format!("Could not read the helper's answer: {e}"))?;
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(response.get("data").cloned().unwrap_or(Value::Null))
    } else {
        Err(response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("The Nimbus helper rejected the request.")
            .to_string())
    }
}

#[cfg(target_os = "windows")]
fn call(request: Value) -> Result<Value, String> {
    use std::io::{BufRead, BufReader, Write};

    // A named pipe is a file to `CreateFileW`, so ordinary file I/O works.
    let mut pipe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(PIPE)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                "App split tunneling needs the Nimbus split tunnel service, which is not installed."
                    .to_string()
            }
            std::io::ErrorKind::PermissionDenied => {
                "The Nimbus split tunnel service refused the connection. Nimbus must run as an \
                 administrator to change the machine-wide exclude list."
                    .to_string()
            }
            _ => format!("Could not reach the Nimbus split tunnel service: {e}"),
        })?;

    let mut payload = serde_json::to_string(&request)
        .map_err(|e| format!("Could not encode the request: {e}"))?;
    payload.push('\n');
    pipe.write_all(payload.as_bytes())
        .map_err(|e| format!("Could not send the request: {e}"))?;

    let mut line = String::new();
    BufReader::new(pipe)
        .read_line(&mut line)
        .map_err(|e| format!("The split tunnel service did not answer: {e}"))?;
    if line.trim().is_empty() {
        return Err("The split tunnel service closed the connection without answering.".into());
    }

    let response: Value =
        serde_json::from_str(&line).map_err(|e| format!("Could not read the answer: {e}"))?;
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(response.get("data").cloned().unwrap_or(Value::Null))
    } else {
        Err(response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("The split tunnel service rejected the request.")
            .to_string())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn call(_request: Value) -> Result<Value, String> {
    Err("App split tunneling is not available on this platform.".into())
}

async fn call_async(request: Value) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || call(request))
        .await
        .map_err(|e| format!("The helper call did not finish: {e}"))?
}

/// Whether the daemon is installed and reachable.
#[tauri::command]
pub fn split_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(SOCKET).exists()
    }
    #[cfg(target_os = "windows")]
    {
        // A pipe has no path to stat; the only way to know is to try to open it.
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(PIPE)
            .is_ok()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        false
    }
}

/// Running applications the calling user owns.
#[tauri::command]
pub async fn split_apps() -> Result<Value, String> {
    call_async(serde_json::json!({ "cmd": "apps" })).await
}

#[tauri::command]
pub async fn split_status() -> Result<Value, String> {
    call_async(serde_json::json!({ "cmd": "status" })).await
}

/// Put the VPN in front of `apps` for the current tunnel.
#[tauri::command]
pub async fn split_begin(apps: Vec<String>, tunnel: Option<String>) -> Result<Value, String> {
    let mut request = serde_json::json!({ "cmd": "begin", "apps": apps });
    if let Some(tunnel) = tunnel.filter(|t| !t.trim().is_empty()) {
        request["tunnel"] = Value::String(tunnel);
    }
    call_async(request).await
}

/// Remove the policy. Must run before Eddie tears the tunnel down.
#[tauri::command]
pub async fn split_end() -> Result<Value, String> {
    call_async(serde_json::json!({ "cmd": "end" })).await
}
