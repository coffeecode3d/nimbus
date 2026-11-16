//! The Windows service: the only privileged part of Nimbus on Windows.
//!
//! On Linux the privileged helper is a systemd unit; here it is a Windows
//! service running as LocalSystem, because the split tunnel driver's control
//! device is exclusive and needs SYSTEM to open. The unprivileged UI talks to it
//! over a named pipe with the same one-request-per-line protocol the Linux
//! helper uses on its Unix socket.
//!
//! The driver handle lives on a single thread. `IOCTL_ST_DEQUEUE_EVENT` parks
//! the request until something happens, so that thread uses a bounded wait
//! (`next_event_timeout`) and drains a command channel in between. Anything that
//! touched the driver from a second thread would either block forever behind a
//! pending dequeue or contend for the exclusive handle.

use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::agent;
use crate::driver::Driver;
use crate::firewall::Firewall;
use crate::pipe::PipeServer;
use crate::proto;
use crate::windows_system::WindowsSystem;

/// The pipe the UI connects to.
pub const PIPE_NAME: &str = r"\\.\pipe\nimbus-split";

/// Service name, as registered with the Service Control Manager.
pub const SERVICE_NAME: &str = "nimbus-split";

/// How long the driver thread waits for an event before checking for commands.
const EVENT_WAIT_MS: u32 = 500;

/// How long the pipe thread waits for the driver thread to answer.
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

static STOP: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
enum Request {
    Ping,
    Apps,
    Status,
    Begin {
        apps: Vec<String>,
    },
    End,
}

enum Command {
    Begin {
        apps: Vec<String>,
        reply: Sender<Result<Value, String>>,
    },
    End {
        reply: Sender<Result<Value, String>>,
    },
    Status {
        reply: Sender<Result<Value, String>>,
    },
}

/// What is currently engaged, plus what it was told to exclude.
struct Engaged {
    driver: Driver,
    /// Kept alive for the lifetime of the engagement: the sublayers the driver's
    /// filters attach to belong to this WFP session.
    _firewall: Firewall,
    apps: Vec<String>,
    tunnel: Option<[u8; 4]>,
    internet: Option<[u8; 4]>,
}

fn status_json(engaged: Option<&Engaged>) -> Value {
    let (apps, tunnel, internet) = match engaged {
        Some(state) => (
            state.apps.clone(),
            state.tunnel.map(std::net::Ipv4Addr::from),
            state.internet.map(std::net::Ipv4Addr::from),
        ),
        None => (Vec::new(), None, None),
    };
    json!({
        "engaged": engaged.is_some(),
        "apps": apps,
        "tunnel": tunnel.map(|ip| ip.to_string()),
        "internet": internet.map(|ip| ip.to_string()),
        "mode": "exclude",
    })
}

/// Engage the driver, excluding `apps` from the tunnel.
fn engage(system: &WindowsSystem, apps: &[String]) -> Result<Engaged, String> {
    let plan = agent::plan(system, apps);
    if plan.images.is_empty() {
        return Err(
            "None of the selected applications could be matched to a running executable."
                .to_string(),
        );
    }

    let firewall = Firewall::open()?;
    let sublayers = proto::SublayerGuids {
        baseline: *b"nimbus-split-bas",
        dns: *b"nimbus-split-dns",
    };
    firewall.ensure_sublayer(sublayers.baseline, "Nimbus split tunnel")?;
    firewall.ensure_sublayer(sublayers.dns, "Nimbus split tunnel DNS")?;

    let driver = Driver::open()?;
    // A previous client may have left the driver mid-sequence.
    if driver.state()? != proto::DriverState::Started {
        driver.reset()?;
    }
    driver.initialize(&sublayers)?;
    driver.register_processes(&plan.tree)?;
    driver.register_addresses(plan.addresses)?;
    driver.set_configuration(&plan.images)?;

    if driver.state()? != proto::DriverState::Engaged {
        // The driver only engages when it has a tunnel address to steer
        // towards; without one it accepts the policy and waits in READY.
        let _ = driver.clear_configuration();
        let _ = driver.reset();
        return Err(
            "The VPN has no tunnel address yet, so applications cannot be excluded. \
             Connect first and try again."
                .to_string(),
        );
    }

    Ok(Engaged {
        driver,
        _firewall: firewall,
        apps: apps.to_vec(),
        tunnel: plan.addresses.tunnel_v4,
        internet: plan.addresses.internet_v4,
    })
}

fn disengage(state: Engaged) {
    // Drop the policy before the handle goes away. If this fails there is
    // nothing useful left to do: the next client resets the driver anyway.
    let _ = state.driver.clear_configuration();
    let _ = state.driver.reset();
}

/// The driver thread: owns the exclusive handle and answers commands between
/// bounded waits for driver events.
fn driver_thread(rx: Receiver<Command>) {
    let system = WindowsSystem::new();
    let mut engaged: Option<Engaged> = None;

    while !STOP.load(Ordering::SeqCst) {
        loop {
            match rx.try_recv() {
                Ok(Command::Begin { apps, reply }) => {
                    if let Some(previous) = engaged.take() {
                        disengage(previous);
                    }
                    log_line(&format!("begin {} app(s): {}", apps.len(), apps.join(", ")));
                    match engage(&system, &apps) {
                        Ok(state) => {
                            let value = status_json(Some(&state));
                            log_line(&format!(
                                "engaged: tunnel={:?} internet={:?}",
                                value["tunnel"], value["internet"]
                            ));
                            engaged = Some(state);
                            let _ = reply.send(Ok(value));
                        }
                        Err(error) => {
                            log_line(&format!("engage failed: {error}"));
                            let _ = reply.send(Err(error));
                        }
                    }
                }
                Ok(Command::End { reply }) => {
                    if let Some(previous) = engaged.take() {
                        log_line("end");
                        disengage(previous);
                    }
                    let _ = reply.send(Ok(Value::Null));
                }
                Ok(Command::Status { reply }) => {
                    let _ = reply.send(Ok(status_json(engaged.as_ref())));
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if let Some(previous) = engaged.take() {
                        disengage(previous);
                    }
                    return;
                }
            }
        }

        // The driver's event queue is deliberately not drained here.
        //
        // `IOCTL_ST_DEQUEUE_EVENT` is an inverted call: the driver stores the
        // request and completes it whenever a process transition happens. Such a
        // request cannot be cancelled from user mode, so a thread that waits on
        // it can never be interrupted — which would leave `end` unanswered. The
        // driver caps its queue at 100 events and discards the oldest, so not
        // reading them costs nothing but detail in the log.
        std::thread::sleep(Duration::from_millis(EVENT_WAIT_MS as u64));
    }

    if let Some(previous) = engaged.take() {
        disengage(previous);
    }
}

/// Where the service records what it did. A service has no console, so without
/// this there is nothing to look at when a policy does not apply; it is also the
/// only way to see the driver's per-process decisions.
const LOG_PATH: &str = r"C:\ProgramData\Nimbus\split.log";

/// Append one line, keeping the file from growing without bound.
fn log_line(line: &str) {
    use std::io::Write;

    let path = std::path::Path::new(LOG_PATH);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(meta) = std::fs::metadata(path)
        && meta.len() > 1_000_000
    {
        let _ = std::fs::remove_file(path);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

/// Running applications, grouped by image path, for the picker.
fn running_apps() -> Value {
    let system = WindowsSystem::new();
    let mut images: Vec<(String, Vec<u32>)> = Vec::new();

    for (pid, win32) in system.running_images() {
        match images
            .iter_mut()
            .find(|(path, _)| path.eq_ignore_ascii_case(&win32))
        {
            Some((_, pids)) => pids.push(pid),
            None => images.push((win32, vec![pid])),
        }
    }

    images.sort_by_key(|(path, _)| path.to_lowercase());
    let list: Vec<Value> = images
        .into_iter()
        .map(|(path, pids)| {
            let name = std::path::Path::new(&path)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone());
            json!({
                "id": path,
                "name": name,
                "icon": Value::Null,
                "exePath": path,
                "pids": pids,
                "uids": [],
                "gui": true,
            })
        })
        .collect();
    Value::Array(list)
}

fn ask<F>(tx: &Sender<Command>, build: F) -> Result<Value, String>
where
    F: FnOnce(Sender<Result<Value, String>>) -> Command,
{
    let (reply, wait) = channel();
    tx.send(build(reply))
        .map_err(|_| "The split tunnel service is stopping.".to_string())?;
    wait.recv_timeout(REPLY_TIMEOUT)
        .map_err(|_| "The split tunnel service did not answer.".to_string())?
}

fn handle_request(request: Request, tx: &Sender<Command>) -> Result<Value, String> {
    match request {
        Request::Ping => Ok(json!("pong")),
        Request::Apps => Ok(running_apps()),
        Request::Status => ask(tx, |reply| Command::Status { reply }),
        Request::Begin { apps } => {
            if apps.is_empty() {
                return Err("No applications were selected.".to_string());
            }
            ask(tx, |reply| Command::Begin { apps, reply })
        }
        Request::End => ask(tx, |reply| Command::End { reply }),
    }
}

/// Serve the pipe until asked to stop. Shared by the service and `--serve`.
fn serve() {
    let (tx, rx) = channel::<Command>();
    let pump = std::thread::spawn(move || driver_thread(rx));

    let server = PipeServer::new(PIPE_NAME);
    while !STOP.load(Ordering::SeqCst) {
        let Some(connection) = server.accept() else {
            std::thread::sleep(Duration::from_millis(200));
            continue;
        };
        let Some(line) = connection.read_line() else {
            continue;
        };
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => match handle_request(request, &tx) {
                Ok(data) => json!({ "ok": true, "data": data }),
                Err(error) => json!({ "ok": false, "error": error }),
            },
            Err(error) => json!({ "ok": false, "error": format!("invalid request: {error}") }),
        };
        let mut payload = response.to_string();
        payload.push('\n');
        connection.write_all(payload.as_bytes());
    }

    STOP.store(true, Ordering::SeqCst);
    drop(tx);
    let _ = pump.join();
}

/// Run the pipe server in the foreground, for testing without the SCM.
pub fn serve_foreground() -> Result<(), String> {
    println!("nimbus-split: serving on {PIPE_NAME}");
    serve();
    Ok(())
}

// --- Installation -----------------------------------------------------------

use windows_sys::Win32::System::Services::{
    CloseServiceHandle, ControlService, CreateServiceW, DeleteService, OpenSCManagerW,
    OpenServiceW, QueryServiceStatus, StartServiceW, SC_MANAGER_ALL_ACCESS, SERVICE_ALL_ACCESS,
    SERVICE_AUTO_START, SERVICE_CONTROL_STOP, SERVICE_ERROR_NORMAL, SERVICE_STATUS,
    SERVICE_STOPPED, SERVICE_WIN32_OWN_PROCESS,
};

/// Register this binary as a service and start it.
///
/// The installer calls this rather than going through `sc.exe`, because the
/// command line contains the installation path, which has spaces in it, and
/// quoting through `schtasks`/`sc`/NSIS is where those things usually break.
pub fn install() -> Result<(), String> {
    let binary = std::env::current_exe()
        .map_err(|e| format!("could not find the service binary: {e}"))?;

    // `sc` and the SCM both want the command line to start with a quoted path.
    let command_line = format!("\"{}\" --service", binary.display());
    let wide = |value: &str| -> Vec<u16> {
        std::ffi::OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };

    let name = wide(SERVICE_NAME);
    let display = wide("Nimbus split tunnel");
    let description_free = wide(&command_line);

    // SAFETY: null machine name means the local SCM; all strings are
    // NUL-terminated and outlive the calls.
    let manager = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_ALL_ACCESS) };
    if manager == 0 {
        return Err(format!(
            "could not open the Service Control Manager (error {}); this needs administrator rights",
            // SAFETY: GetLastError has no preconditions.
            unsafe { GetLastError() }
        ));
    }

    // SAFETY: arguments are valid and NUL-terminated.
    let service = unsafe {
        CreateServiceW(
            manager,
            name.as_ptr(),
            display.as_ptr(),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            description_free.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };

    // An upgrade finds the service already there; use it and just (re)start it.
    let service = if service == 0 {
        // SAFETY: GetLastError has no preconditions.
        let error = unsafe { GetLastError() };
        if error != 1073 {
            // ERROR_SERVICE_EXISTS is what an existing install reports.
            // SAFETY: the manager handle came from the SCM.
            unsafe { CloseServiceHandle(manager) };
            return Err(format!("could not create the service (error {error})"));
        }
        // SAFETY: valid handle and NUL-terminated name.
        unsafe { OpenServiceW(manager, name.as_ptr(), SERVICE_ALL_ACCESS) }
    } else {
        service
    };

    if service == 0 {
        // SAFETY: GetLastError has no preconditions.
        let error = unsafe { GetLastError() };
        // SAFETY: the manager handle came from the SCM.
        unsafe { CloseServiceHandle(manager) };
        return Err(format!("could not open the existing service (error {error})"));
    }

    // SAFETY: `service` is a valid handle from CreateServiceW.
    let started = unsafe { StartServiceW(service, 0, std::ptr::null()) } != 0;
    // SAFETY: GetLastError has no preconditions.
    let error = unsafe { GetLastError() };
    // SAFETY: both handles came from the SCM and are closed once.
    unsafe {
        CloseServiceHandle(service);
        CloseServiceHandle(manager);
    }

    if !started && error != 1056 {
        // 1056 is ERROR_SERVICE_ALREADY_RUNNING.
        return Err(format!("the service was installed but did not start (error {error})"));
    }
    println!("{SERVICE_NAME} installed");
    Ok(())
}

/// Stop and remove the service. The driver is left alone: it is shared with
/// anything else that may use it, and the installer removes it explicitly.
pub fn uninstall() -> Result<(), String> {
    let name: Vec<u16> = SERVICE_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: null machine name means the local SCM.
    let manager = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_ALL_ACCESS) };
    if manager == 0 {
        return Err("could not open the Service Control Manager".to_string());
    }
    // SAFETY: valid, NUL-terminated name.
    let service = unsafe { OpenServiceW(manager, name.as_ptr(), SERVICE_ALL_ACCESS) };
    if service == 0 {
        // SAFETY: the manager handle came from the SCM.
        unsafe { CloseServiceHandle(manager) };
        return Ok(()); // nothing installed
    }

    let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: a valid handle and a properly sized status struct.
    unsafe { ControlService(service, SERVICE_CONTROL_STOP, &mut status) };
    // SAFETY: a valid handle from OpenServiceW.
    unsafe { DeleteService(service) };

    // Wait for the process to exit: it still holds the binary open, and the
    // installer will want to replace that file next.
    for _ in 0..50 {
        let mut now: SERVICE_STATUS = unsafe { std::mem::zeroed() };
        // SAFETY: a valid handle and a properly sized status struct.
        if unsafe { QueryServiceStatus(service, &mut now) } == 0 {
            break;
        }
        if now.dwCurrentState == SERVICE_STOPPED {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    unsafe {
        CloseServiceHandle(service);
        CloseServiceHandle(manager);
    }
    println!("{SERVICE_NAME} removed");
    Ok(())
}

// --- Service Control Manager plumbing ---------------------------------------

use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::System::Services::{
    RegisterServiceCtrlHandlerW, SERVICE_ACCEPT_STOP, SERVICE_RUNNING,
    SERVICE_START_PENDING, SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING, SERVICE_TABLE_ENTRYW,
    SetServiceStatus, StartServiceCtrlDispatcherW,
};

/// Set by `service_main` before any state change is reported.
static STATUS_HANDLE: AtomicIsize = AtomicIsize::new(0);

fn set_state(state: u32, checkpoint: u32) {
    let handle = STATUS_HANDLE.load(Ordering::SeqCst);
    if handle == 0 {
        return;
    }
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP
        } else {
            0
        },
        dwWin32ExitCode: 0,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: checkpoint,
        dwWaitHint: 0,
    };
    // SAFETY: the handle came from RegisterServiceCtrlHandlerW.
    unsafe { SetServiceStatus(handle as SERVICE_STATUS_HANDLE, &status) };
}

unsafe extern "system" fn control_handler(control: u32) {
    if control == SERVICE_CONTROL_STOP {
        STOP.store(true, Ordering::SeqCst);
        set_state(SERVICE_STOP_PENDING, 1);
        // `serve` is parked in ConnectNamedPipe, which does not notice a flag.
        // A throwaway connection makes it return so the loop can see STOP and
        // the process can exit; otherwise the service hangs in STOP_PENDING and
        // keeps its image locked, so upgrades cannot replace the binary.
        std::thread::spawn(|| {
            if let Ok(pipe) = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(PIPE_NAME)
            {
                drop(pipe);
            }
        });
    }
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
    let name: Vec<u16> = SERVICE_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: the name is NUL-terminated and outlives the call.
    let handle = unsafe { RegisterServiceCtrlHandlerW(name.as_ptr(), Some(control_handler)) };
    if handle == 0 {
        return;
    }
    STATUS_HANDLE.store(handle as isize, Ordering::SeqCst);

    set_state(SERVICE_START_PENDING, 0);
    set_state(SERVICE_RUNNING, 0);

    log_line("service starting");
    serve();
    log_line("service stopping");

    set_state(SERVICE_STOPPED, 0);
}

/// Connect to the SCM and run as a service. Blocks until the service stops.
pub fn run_service() -> Result<(), String> {
    let name: Vec<u16> = SERVICE_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_ptr() as *mut u16,
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: std::ptr::null_mut(),
            lpServiceProc: None,
        },
    ];

    // SAFETY: the table is NUL-terminated and lives for the call.
    let ok = unsafe { StartServiceCtrlDispatcherW(table.as_ptr() as *const SERVICE_TABLE_ENTRYW) };
    if ok == 0 {
        // SAFETY: GetLastError has no preconditions.
        let error = unsafe { GetLastError() };
        return Err(format!(
            "could not connect to the Service Control Manager (error {error}); \
             this binary must be started by the SCM with --service"
        ));
    }
    Ok(())
}
