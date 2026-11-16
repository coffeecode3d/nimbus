//! Unix-socket IPC for the long-lived helper daemon.
//!
//! Nimbus connects to `/run/nimbus/helper.sock` and exchanges one JSON object
//! per line. The daemon is a systemd service running as root; the caller is
//! whoever owns the connecting socket, read with `SO_PEERCRED`. That uid is the
//! *only* authority the caller gets: it may list its own applications, and it
//! may steer its own processes. It can never touch another user's session.
//!
//! ```text
//! {"cmd":"apps"}                                  → [App, …]
//! {"cmd":"status"}                                → Status + owned
//! {"cmd":"begin","apps":["helium-browser"],"tunnel":"Eddie"} → Status
//! {"cmd":"end"}                                   → null
//! {"cmd":"ping"}                                  → "pong"
//! ```

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{apps, session};

/// Where the systemd unit puts the socket. `RuntimeDirectory=nimbus` in the
/// unit creates `/run/nimbus` with the right ownership.
pub const DEFAULT_SOCKET: &str = "/run/nimbus/helper.sock";

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum Request {
    Ping,
    Apps,
    Status,
    Begin {
        apps: Vec<String>,
        #[serde(default)]
        tunnel: Option<String>,
    },
    End,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    fn ok(data: serde_json::Value) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(message.into()),
        }
    }
}

#[derive(Default)]
pub struct Daemon {
    session: Mutex<Option<session::Session>>,
    owner: Mutex<Option<u32>>,
}

/// An app id is an executable basename from `/proc`. Reject anything that is
/// not a plausible single path component before it reaches a command.
fn validate_app_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 255 {
        return Err(format!("invalid app id {id:?}"));
    }
    if id.contains('/') || id.contains('\0') || id.starts_with('.') {
        return Err(format!("invalid app id {id:?}"));
    }
    Ok(())
}

impl Daemon {
    fn handle(&self, request: Request, uid: u32) -> Response {
        match request {
            Request::Ping => Response::ok(serde_json::json!("pong")),
            Request::Apps => match serde_json::to_value(apps::list_for(Some(uid))) {
                Ok(list) => Response::ok(list),
                Err(e) => Response::err(format!("could not encode apps: {e}")),
            },
            Request::Status => self.status(uid),
            Request::Begin { apps, tunnel } => self.begin(apps, tunnel, uid),
            Request::End => self.end(uid),
        }
    }

    fn status(&self, uid: u32) -> Response {
        let owned = *self.owner.lock().unwrap() == Some(uid);
        let mut value = match serde_json::to_value(session::status()) {
            Ok(value) => value,
            Err(e) => return Response::err(format!("could not encode status: {e}")),
        };
        if let Some(map) = value.as_object_mut() {
            map.insert("owned".into(), serde_json::Value::Bool(owned));
        }
        Response::ok(value)
    }

    fn begin(&self, app_ids: Vec<String>, tunnel: Option<String>, uid: u32) -> Response {
        if app_ids.is_empty() {
            return Response::err("no apps selected");
        }
        if let Err(e) = app_ids.iter().try_for_each(|id| validate_app_id(id)) {
            return Response::err(e);
        }

        // One session at a time, and only its owner may replace it.
        if let Some(current) = *self.owner.lock().unwrap()
            && current != uid
        {
            return Response::err("another user has an active split tunneling session");
        }

        let tunnel = match tunnel.or_else(session::detect_tunnel) {
            Some(tunnel) => tunnel,
            None => return Response::err("no tunnel device detected; connect the VPN first"),
        };
        if !Path::new(&format!("/sys/class/net/{tunnel}")).exists() {
            return Response::err(format!("tunnel device {tunnel} does not exist"));
        }

        // Replace any stale session before installing the new one.
        if let Some(mut previous) = self.session.lock().unwrap().take() {
            previous.teardown();
        }

        match session::Session::begin(&tunnel, &app_ids, Some(uid)) {
            Ok(session) => {
                *self.session.lock().unwrap() = Some(session);
                *self.owner.lock().unwrap() = Some(uid);
                self.status(uid)
            }
            Err(e) => Response::err(e),
        }
    }

    fn end(&self, uid: u32) -> Response {
        if let Some(owner) = *self.owner.lock().unwrap()
            && owner != uid
            && uid != 0
        {
            return Response::err("another user owns the active session");
        }
        if let Some(mut session) = self.session.lock().unwrap().take() {
            session.teardown();
        }
        *self.owner.lock().unwrap() = None;
        Response::ok(serde_json::Value::Null)
    }
}

/// Watch for the tunnel disappearing, so a dropped VPN cannot leave a policy
/// rule and a mark pointing at a device that no longer exists.
fn spawn_reaper(state: Arc<Daemon>) {
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_secs(1));

            let tunnel = match state.session.lock().unwrap().as_ref() {
                Some(session) => session.tunnel.clone(),
                None => return, // session ended normally
            };
            if Path::new(&format!("/sys/class/net/{tunnel}")).exists() {
                continue;
            }

            eprintln!("nimbus-helper: tunnel {tunnel} disappeared, removing policy");
            if let Some(mut session) = state.session.lock().unwrap().take() {
                session.teardown();
            }
            *state.owner.lock().unwrap() = None;
            return;
        }
    });
}

/// Run the daemon. Blocks until the process is killed.
pub fn serve(socket_path: &str) -> Result<(), String> {
    if let Some(dir) = Path::new(socket_path).parent() {
        fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    let _ = fs::remove_file(socket_path);

    let listener = UnixListener::bind(socket_path)
        .map_err(|e| format!("could not bind {socket_path}: {e}"))?;
    // Any local user may connect; what they may *do* is decided per request
    // from SO_PEERCRED, not from filesystem permissions.
    let _ = fs::set_permissions(socket_path, fs::Permissions::from_mode(0o666));

    let state = Arc::new(Daemon::default());
    println!("nimbus-helper: listening on {socket_path}");

    // Non-blocking accept so SIGTERM is noticed promptly: `systemctl stop` must
    // not hang, and stopping has to remove the policy, not just exit.
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("could not set the socket non-blocking: {e}"))?;

    while !crate::signal::stopping() {
        match listener.accept() {
            Ok((stream, _)) => {
                let state = Arc::clone(&state);
                thread::spawn(move || handle_connection(stream, state));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(200));
            }
            Err(e) => eprintln!("nimbus-helper: accept failed: {e}"),
        }
    }

    println!("nimbus-helper: shutting down, removing policy");
    if let Some(mut session) = state.session.lock().unwrap().take() {
        session.teardown();
    }
    let _ = fs::remove_file(socket_path);
    Ok(())
}

/// Read `SO_PEERCRED` from a connected socket.
///
/// `std`'s `peer_cred` is still unstable, so this goes through libc directly.
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    use std::os::unix::io::AsRawFd;

    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the fd is valid for the lifetime of `stream`, and the buffer is
    // exactly the size the kernel expects for SO_PEERCRED.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    if rc == 0 { Some(cred.uid) } else { None }
}

fn handle_connection(stream: UnixStream, state: Arc<Daemon>) {
    // The kernel tells us who connected; never trust a uid in the payload.
    let uid = match peer_uid(&stream) {
        Some(uid) => uid,
        None => {
            eprintln!("nimbus-helper: could not read peer credentials");
            return;
        }
    };
    let reader = match stream.try_clone() {
        Ok(clone) => BufReader::new(clone),
        Err(_) => return,
    };
    let mut writer = stream;

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }

        let (response, started) = match serde_json::from_str::<Request>(&line) {
            Ok(request) => {
                let started = matches!(request, Request::Begin { .. });
                (state.handle(request, uid), started)
            }
            Err(e) => (Response::err(format!("bad request: {e}")), false),
        };

        let mut payload =
            serde_json::to_string(&response).unwrap_or_else(|_| r#"{"ok":false}"#.into());
        payload.push('\n');
        if writer.write_all(payload.as_bytes()).is_err() {
            break;
        }

        // Start the tunnel watchdog after a successful begin. It owns its own
        // Arc so it outlives this connection: the UI may disconnect the socket
        // while the session keeps running.
        if started && response.ok {
            spawn_reaper(Arc::clone(&state));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_ids_must_be_single_components() {
        assert!(validate_app_id("helium-browser").is_ok());
        assert!(validate_app_id("zoom").is_ok());
        assert!(validate_app_id("").is_err());
        assert!(validate_app_id("/usr/bin/foo").is_err());
        assert!(validate_app_id("../etc/passwd").is_err());
        assert!(validate_app_id("a\0b").is_err());
        assert!(validate_app_id(&"x".repeat(300)).is_err());
    }

    #[test]
    fn request_tag_is_lowercase() {
        let request: Request = serde_json::from_str(r#"{"cmd":"status"}"#).unwrap();
        assert!(matches!(request, Request::Status));

        let request: Request =
            serde_json::from_str(r#"{"cmd":"begin","apps":["a"],"tunnel":"Eddie"}"#).unwrap();
        match request {
            Request::Begin { apps, tunnel } => {
                assert_eq!(apps, vec!["a"]);
                assert_eq!(tunnel.as_deref(), Some("Eddie"));
            }
            _ => panic!("wrong variant"),
        }

        let request: Request = serde_json::from_str(r#"{"cmd":"begin","apps":["a"]}"#).unwrap();
        assert!(matches!(request, Request::Begin { tunnel: None, .. }));
    }
}
