use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, State};

/// Everything the frontend needs to describe the engine environment.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineInfo {
    pub found: bool,
    pub cli: String,
    pub app_dir: String,
    pub profile: String,
    pub error: Option<String>,
}

impl EngineInfo {
    fn missing(error: impl Into<String>) -> Self {
        Self {
            found: false,
            cli: String::new(),
            app_dir: String::new(),
            profile: profile_dir().display().to_string(),
            error: Some(error.into()),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartResult {
    pub info: EngineInfo,
    pub session: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StartOptions {
    /// AirVPN server code (public name), e.g. "Achernar".
    pub server: Option<String>,
    /// "wireguard" | "openvpn" | "auto"
    pub protocol: Option<String>,
    /// "udp" | "tcp"
    pub mode: Option<String>,
    pub port: Option<u16>,
    pub netlock: Option<bool>,
    /// Optional caller-chosen event stream tag. The UI sets this before the
    /// spawn so it can receive `engine.ready` even if it arrives before the
    /// command's response. Falls back to the process generation.
    pub session: Option<String>,
}

struct Process {
    child: Child,
    stdin: Option<ChildStdin>,
    profile: PathBuf,
}

pub struct EngineState {
    process: Mutex<Option<Process>>,
    info: Mutex<Option<EngineInfo>>,
    generation: AtomicU64,
}

impl Default for EngineState {
    fn default() -> Self {
        Self {
            process: Mutex::new(None),
            info: Mutex::new(None),
            generation: AtomicU64::new(0),
        }
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// A profile owned by Nimbus. Never point this at ~/.config/eddie: the engine
/// rewrites its profile on shutdown, and an unreadable one is replaced with
/// defaults.
pub fn profile_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("NIMBUS_PROFILE") {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".local/share"));
    base.join("nimbus/profile")
}

fn is_cli(path: &Path) -> bool {
    path.is_file()
}

/// The privileged helper must share a directory with its client. When Eddie is
/// already running, the helper's path tells us which install is live.
fn service_dir() -> Option<PathBuf> {
    let entries = std::fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        for arg in cmdline.split(|b| *b == 0) {
            let arg = String::from_utf8_lossy(arg);
            if arg.ends_with("/eddie-cli-elevated") {
                if let Some(dir) = Path::new(arg.as_ref()).parent() {
                    if is_cli(&dir.join("eddie-cli")) {
                        return Some(dir.to_path_buf());
                    }
                }
            }
        }
    }
    None
}

fn mount_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/tmp") else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(".mount_") && !name.contains("eddie") {
            continue;
        }
        let base = entry.path();
        // /tmp/.mount_eddieXXXX/opt/eddie-ui
        let direct = base.join("opt/eddie-ui");
        if is_cli(&direct.join("eddie-cli")) {
            out.push(direct);
        }
        // Extracted AppImages / dev trees: /tmp/<dir>/<dir>/opt/eddie-ui
        if let Ok(children) = std::fs::read_dir(&base) {
            for child in children.flatten() {
                let nested = child.path().join("opt/eddie-ui");
                if is_cli(&nested.join("eddie-cli")) {
                    out.push(nested);
                }
            }
        }
    }
    out
}

fn find_cli() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("NIMBUS_EDDIE_CLI") {
        let p = PathBuf::from(path);
        if is_cli(&p) {
            return Some(p);
        }
    }
    if let Some(dir) = service_dir() {
        return Some(dir.join("eddie-cli"));
    }
    for dir in [
        "/opt/eddie-ui",
        "/usr/lib/eddie-ui",
        "/usr/local/lib/eddie-ui",
        "/usr/share/eddie-ui",
    ] {
        let p = Path::new(dir).join("eddie-cli");
        if is_cli(&p) {
            return Some(p);
        }
    }
    // A stable extracted AppImage is preferable to a /tmp mount: the mounted
    // path changes on every launch, which breaks Eddie's installed helper.
    let home = home_dir();
    for rel in [
        ".local/opt/eddie/squashfs-root/opt/eddie-ui",
        ".local/opt/eddie/opt/eddie-ui",
        ".local/opt/Eddie/squashfs-root/opt/eddie-ui",
    ] {
        let p = home.join(rel).join("eddie-cli");
        if is_cli(&p) {
            return Some(p);
        }
    }
    for dir in mount_candidates() {
        let p = dir.join("eddie-cli");
        if is_cli(&p) {
            return Some(p);
        }
    }
    None
}

pub(crate) fn locate() -> EngineInfo {
    let Some(cli) = find_cli() else {
        return EngineInfo::missing(
            "Could not find eddie-cli. Install the official Eddie package, or set NIMBUS_EDDIE_CLI.",
        );
    };
    let Some(dir) = cli.parent() else {
        return EngineInfo::missing("eddie-cli has no parent directory.");
    };
    EngineInfo {
        found: true,
        cli: cli.display().to_string(),
        app_dir: dir.display().to_string(),
        profile: profile_dir().display().to_string(),
        error: None,
    }
}

fn flag(name: &str, value: impl std::fmt::Display) -> String {
    format!("--{name}={value}")
}

fn build_args(options: &StartOptions, profile: &Path) -> Vec<String> {
    let mut args = vec![
        "--jsoninout".to_string(),
        flag("path", profile.display()),
        // Always start idle. Nimbus sends the account credentials over stdin
        // (`options.set`) once the engine is ready, then triggers
        // `mainaction.connect`. That keeps the password out of the process
        // command line, where any local user could read it via /proc.
        "--connect=false".to_string(),
        // Eddie defaults to remember=false, which calls DeAuth() on a clean
        // shutdown and would erase the saved session between our restarts.
        "--remember=true".to_string(),
    ];

    if let Some(server) = options.server.as_deref().filter(|s| !s.is_empty()) {
        args.push(flag("server", server));
    }
    if let Some(protocol) = options.protocol.as_deref().filter(|s| !s.is_empty()) {
        args.push(flag("mode.type", protocol));
    }
    if let Some(mode) = options.mode.as_deref().filter(|s| !s.is_empty()) {
        args.push(flag("mode.protocol", mode));
    }
    if let Some(port) = options.port {
        args.push(flag("mode.port", port));
    }
    // Network lock is explicit: when it is off we also disable the mode and
    // the session-level switch, otherwise a value left in the profile (or the
    // recovery state) can arm the firewall on an idle engine and cut the
    // machine off before the provider authorization request can complete.
    if options.netlock == Some(true) {
        args.push("--netlock.mode=auto".to_string());
        args.push("--netlock=true".to_string());
        // The provider API is contacted after the lock is up; without DNS the
        // authorization request can hang until the session is cancelled.
        args.push("--netlock.allow_dns=true".to_string());
    } else {
        args.push("--netlock.mode=none".to_string());
        args.push("--netlock=false".to_string());
        args.push("--netlock.connection=false".to_string());
    }
    args
}

/// Never let a credential reach a terminal or log file. Eddie echoes its own
/// command line, which can contain `password="..."` — either raw or escaped
/// inside a JSON log message (`password=\"...\"`).
fn redact(line: &str) -> String {
    // Eddie's log lines are JSON, so the echoed command line has escaped
    // quotes. Normalise them before scanning; the trace is human-readable.
    let normalized = line.replace("\\\"", "\"");
    let mut out = String::with_capacity(normalized.len());
    let mut rest = normalized.as_str();
    while let Some(idx) = rest.find("password=\"") {
        let (head, tail) = rest.split_at(idx + "password=\"".len());
        out.push_str(head);
        match tail.find('"') {
            Some(end) => {
                out.push_str("***\"");
                rest = &tail[end + 1..];
            }
            None => {
                out.push_str("***");
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Eddie emits secret profile changes as
/// `{"command":"option.change","name":"password","value":"..."}`.
/// Redact that JSON value on the way to both the trace log and the webview,
/// without normalising the rest of the line (it has to stay valid JSON).
fn redact_json_password(line: &str) -> String {
    let Some(name_at) = line.find("\"name\":\"password\"") else {
        return line.to_string();
    };
    let after = &line[name_at..];
    let Some(value_rel) = after.find("\"value\":\"") else {
        return line.to_string();
    };
    let value_start = name_at + value_rel + "\"value\":\"".len();
    let bytes = line.as_bytes();
    let mut end = value_start;
    while end < bytes.len() {
        if bytes[end] == b'"' {
            let mut backslashes = 0;
            let mut back = end;
            while back > value_start && bytes[back - 1] == b'\\' {
                backslashes += 1;
                back -= 1;
            }
            if backslashes % 2 == 0 {
                break;
            }
        }
        end += 1;
    }
    if end >= bytes.len() {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    out.push_str(&line[..value_start]);
    out.push_str("***");
    out.push_str(&line[end..]);
    out
}

/// Eddie's `remember=true` mode saves every non-empty profile option, including
/// the password. Nimbus keeps the authoritative copy in the OS keyring, so ask
/// Eddie to clear it before the shutdown save. A hard crash can still leave an
/// encrypted copy in the profile; it is never written in plaintext.
const CLEAR_PASSWORD: &[u8] =
    b"{\"command\":\"options.set\",\"name\":\"password\",\"value\":\"\"}\n";

fn stop_locked(slot: &mut Option<Process>) {
    if let Some(mut process) = slot.take() {
        // `exit` is the command that runs Eddie's shutdown path: it writes the
        // encrypted profile (including the provider cache) and broadcasts
        // engine.shutdown. The CLI process then keeps waiting on stdin, so
        // watch for the profile to be written, then terminate.
        let before = std::fs::metadata(&process.profile)
            .and_then(|meta| meta.modified())
            .ok();
        if let Some(mut stdin) = process.stdin.take() {
            let _ = stdin.write_all(CLEAR_PASSWORD);
            let _ = stdin.write_all(b"{\"command\":\"exit\"}\n");
            let _ = stdin.flush();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(4500);
        while std::time::Instant::now() < deadline {
            if let Ok(Some(_)) = process.child.try_wait() {
                return;
            }
            let saved = std::fs::metadata(&process.profile)
                .and_then(|meta| meta.modified())
                .map(|modified| before.map_or(true, |old| modified > old))
                .unwrap_or(false);
            if saved {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        unsafe {
            libc::kill(process.child.id() as i32, libc::SIGTERM);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1200);
        while std::time::Instant::now() < deadline {
            match process.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
                Err(_) => break,
            }
        }
        let _ = process.child.kill();
        let _ = process.child.wait();
    }
}

/// Stop the engine on app exit so Eddie can save its profile and release the
/// network lock.
pub fn shutdown(state: &EngineState) {
    let mut slot = state.process.lock().unwrap();
    stop_locked(&mut slot);
}

/// Best-effort firewall release. Eddie exposes no netlock command over the
/// jsoninout API, and a session that is stuck in the provider authorization
/// call may never process `exit`. Starting a fresh engine with the lock
/// disabled makes Eddie load its recovery file and tear the iptables rules
/// down before it becomes idle; the short-lived engine is then stopped again.
#[tauri::command]
pub async fn engine_disarm(app: AppHandle) -> Result<AuthResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        let state = app.state::<EngineState>();
        disarm_inner(state.inner())
    })
    .await
    .map_err(|e| e.to_string())?
}

fn disarm_inner(state: &EngineState) -> Result<AuthResult, String> {
    let info = locate();
    if !info.found {
        return Ok(AuthResult {
            ok: false,
            message: info
                .error
                .unwrap_or_else(|| "eddie-cli not found".into()),
        });
    }
    let profile = PathBuf::from(&info.profile);
    std::fs::create_dir_all(&profile).map_err(|e| format!("Cannot create profile dir: {e}"))?;

    // Hold the engine slot for the whole cleanup run. A connect that arrives
    // while the lock is being released waits here instead of spawning a second
    // engine (Eddie refuses to run two at once).
    let mut slot = state.process.lock().unwrap();
    stop_locked(&mut slot);
    kill_orphan(&profile);

    let args = vec![
        "--jsoninout".to_string(),
        flag("path", profile.display()),
        "--connect=false".to_string(),
        "--remember=true".to_string(),
        "--netlock.mode=none".to_string(),
        "--netlock=false".to_string(),
        "--netlock.connection=false".to_string(),
    ];

    let mut command = Command::new(&info.cli);
    command
        .current_dir(&info.app_dir)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }

    let mut child = command
        .spawn()
        .map_err(|e| format!("Could not start eddie-cli: {e}"))?;
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take();

    let (tx, rx) = std::sync::mpsc::channel::<String>();
    if let Some(stdout) = stdout {
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut ready = false;
    let mut recovery_at: Option<std::time::Instant> = None;
    while std::time::Instant::now() < deadline {
        // `Recovery.Load()` runs at the "Check recovery status" step, well
        // before Eddie refreshes provider data. If the old lock is blocking the
        // network, that refresh can hang; give recovery a moment and leave.
        if let Some(at) = recovery_at {
            if at.elapsed() >= std::time::Duration::from_millis(2000) {
                ready = true;
                break;
            }
        }
        match rx.recv_timeout(std::time::Duration::from_millis(250)) {
            Ok(line) => {
                if line.contains("\"engine.ready\"") {
                    ready = true;
                    break;
                } else if line.contains("Another instance") {
                    ready = false;
                    break;
                } else if line.contains("Check recovery status") {
                    recovery_at = Some(std::time::Instant::now());
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    if let Some(stdin) = stdin.as_mut() {
        let _ = stdin.write_all(CLEAR_PASSWORD);
        let _ = stdin.write_all(b"{\"command\":\"exit\"}\n");
        let _ = stdin.flush();
    }
    let stop_deadline = std::time::Instant::now() + std::time::Duration::from_millis(4500);
    while std::time::Instant::now() < stop_deadline {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if child.try_wait().ok().flatten().is_none() {
        unsafe {
            libc::kill(child.id() as i32, libc::SIGTERM);
        }
        std::thread::sleep(std::time::Duration::from_millis(800));
    }
    let _ = child.kill();
    let _ = child.wait();

    Ok(AuthResult {
        ok: ready,
        message: if ready {
            "Network lock released.".into()
        } else {
            "Could not start Eddie to release the network lock. Reboot if your network is still blocked.".into()
        },
    })
}

/// Eddie's jsoninout API has no login command: the UI is expected to call
/// `Engine.Login()` in-process, which a CLI client cannot do. But the normal
/// console client does exactly that on ready, and writes the authenticated
/// `<user>` node (keys, certificates, expiry) into the profile. So sign-in is a
/// short-lived console run against Nimbus's own profile; subsequent jsoninout
/// sessions load that user and can connect without a password.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthResult {
    pub ok: bool,
    pub message: String,
}

#[tauri::command]
pub async fn engine_authenticate(
    app: AppHandle,
    login: String,
    password: String,
) -> Result<AuthResult, String> {
    // The console login blocks for several seconds. Keep it off the webview
    // thread so the "Signing in…" spinner keeps moving.
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        let state = app.state::<EngineState>();
        authenticate_inner(state.inner(), login, password)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn authenticate_inner(
    state: &EngineState,
    login: String,
    password: String,
) -> Result<AuthResult, String> {
    let info = locate();
    if !info.found {
        return Err(info
            .error
            .clone()
            .unwrap_or_else(|| "eddie-cli not found".into()));
    }
    if login.trim().is_empty() || password.is_empty() {
        return Ok(AuthResult {
            ok: false,
            message: "Enter both your AirVPN username and password.".into(),
        });
    }

    let profile = PathBuf::from(&info.profile);
    std::fs::create_dir_all(&profile).map_err(|e| format!("Cannot create profile dir: {e}"))?;
    std::fs::create_dir_all(profile.join("tmp")).ok();

    // Eddie's single-instance lock is global for the app id, so the UI engine
    // must be gone before the console client starts.
    {
        let mut slot = state.process.lock().unwrap();
        stop_locked(&mut slot);
    }
    kill_orphan(&profile);

    let args = vec![
        flag("path", profile.display()),
        flag("connect", "false"),
        "--netlock.mode=none".to_string(),
        "--netlock=false".to_string(),
        "--netlock.connection=false".to_string(),
        // Keep the authenticated user (keys/certificates) in the profile when
        // the engine exits; otherwise Eddie's default `remember=false` deauths
        // on every clean shutdown and the next session is back to square one.
        "--remember=true".to_string(),
        "--advanced.expert=true".to_string(),
        flag("login", login.trim()),
        flag("password", &password),
    ];

    let mut command = Command::new(&info.cli);
    command
        .current_dir(&info.app_dir)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }

    let mut child = command
        .spawn()
        .map_err(|e| format!("Could not start eddie-cli: {e}"))?;
    let stdout = child.stdout.take();

    let (tx, rx) = std::sync::mpsc::channel::<String>();
    if let Some(stdout) = stdout {
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
    let mut result: Option<AuthResult> = None;
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(std::time::Duration::from_millis(250)) {
            Ok(line) => {
                if line.contains("Logged in.") {
                    result = Some(AuthResult {
                        ok: true,
                        message: "Signed in. The session is saved in Nimbus's profile.".into(),
                    });
                    break;
                }
                if line.contains("Wrong login/password") {
                    result = Some(AuthResult {
                        ok: false,
                        message: "Wrong AirVPN username or password.".into(),
                    });
                    break;
                }
                if line.contains("Cannot login") {
                    result = Some(AuthResult {
                        ok: false,
                        message: line
                            .split(" - ")
                            .last()
                            .unwrap_or("Eddie could not sign in.")
                            .trim()
                            .to_string(),
                    });
                    break;
                }
                if line.contains("Another instance") || line.contains("already running") {
                    result = Some(AuthResult {
                        ok: false,
                        message: "Another Eddie instance is running. Quit it and try again.".into(),
                    });
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // On success Eddie writes the profile after logging "Logged in."; give it a
    // moment to finish encrypting and saving before terminating the process.
    if result.as_ref().is_some_and(|r| r.ok) {
        std::thread::sleep(std::time::Duration::from_millis(1500));
    }

    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();

    Ok(result.unwrap_or_else(|| AuthResult {
        ok: false,
        message: "Eddie did not finish signing in. Check your network and try again.".into(),
    }))
}

/// Recover from a previous crash. Eddie's single-instance lock is a pid file in
/// the temp dir; if the pid belongs to an engine pointed at *our* profile, it is
/// an orphan from a killed Nimbus process and can be stopped safely.
fn kill_orphan(profile: &Path) {
    let marker = format!("--path={}", profile.display());
    let mut dirs = vec![std::env::temp_dir(), PathBuf::from("/tmp")];
    dirs.sort();
    dirs.dedup();

    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !(name.starts_with("Eddie_") && name.ends_with(".pid")) {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let Ok(pid) = contents.trim().parse::<i32>() else {
                let _ = std::fs::remove_file(entry.path());
                continue;
            };
            let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
                let _ = std::fs::remove_file(entry.path());
                continue;
            };
            let cmdline = String::from_utf8_lossy(&cmdline);
            if !cmdline.contains(&marker) {
                continue;
            }
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(3000);
            while std::time::Instant::now() < deadline {
                if !Path::new(&format!("/proc/{pid}")).exists() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if Path::new(&format!("/proc/{pid}")).exists() {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[tauri::command]
pub fn engine_info(state: State<'_, EngineState>) -> EngineInfo {
    if let Some(info) = state.info.lock().unwrap().clone() {
        return info;
    }
    locate()
}

#[tauri::command]
pub async fn engine_start(app: AppHandle, options: StartOptions) -> Result<StartResult, String> {
    // Stopping the previous engine can take a few seconds if it is wedged.
    // Run the whole spawn sequence on a blocking worker so the UI can animate
    // the connecting state instead of freezing.
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        let state = handle.state::<EngineState>();
        start_inner(handle.clone(), state.inner(), options)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn start_inner(
    app: AppHandle,
    state: &EngineState,
    options: StartOptions,
) -> Result<StartResult, String> {
    let info = locate();
    if !info.found {
        *state.info.lock().unwrap() = Some(info.clone());
        return Err(info
            .error
            .clone()
            .unwrap_or_else(|| "eddie-cli not found".into()));
    }

    let profile = PathBuf::from(&info.profile);
    std::fs::create_dir_all(&profile).map_err(|e| format!("Cannot create profile dir: {e}"))?;
    std::fs::create_dir_all(profile.join("tmp")).ok();

    {
        let mut slot = state.process.lock().unwrap();
        stop_locked(&mut slot);
    }
    kill_orphan(&profile);

    let generation = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    let session = options
        .session
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("e{generation}"));
    let args = build_args(&options, &profile);

    let mut command = Command::new(&info.cli);
    command
        .current_dir(&info.app_dir)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // If the UI process dies unexpectedly, take the engine with it instead of
    // leaving an orphan that holds Eddie's single-instance lock.
    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }

    let mut child = command.spawn().map_err(|e| {
        let msg = format!("Could not start eddie-cli: {e}");
        *state.info.lock().unwrap() = Some(EngineInfo {
            error: Some(msg.clone()),
            ..info.clone()
        });
        msg
    })?;

    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    *state.process.lock().unwrap() = Some(Process {
        child,
        stdin,
        profile: profile.clone(),
    });
    *state.info.lock().unwrap() = Some(info.clone());

    if let Some(stdout) = stdout {
        let app = app.clone();
        let session = session.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if line.trim().is_empty() {
                    continue;
                }
                let line = redact_json_password(&line);
                if std::env::var_os("NIMBUS_TRACE").is_some() {
                    eprintln!("[engine-out] {}", redact(&line));
                }
                let _ = app.emit(
                    "engine://line",
                    serde_json::json!({ "session": session, "line": line }),
                );
            }
            let _ = app.emit("engine://eof", session);
        });
    }

    if let Some(stderr) = stderr {
        let app = app.clone();
        let session = session.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    let line = redact_json_password(&line);
                    if std::env::var_os("NIMBUS_TRACE").is_some() {
                        eprintln!("[engine-err] {}", redact(&line));
                    }
                    let _ = app.emit(
                        "engine://stderr",
                        serde_json::json!({ "session": session, "line": line }),
                    );
                }
            }
        });
    }

    Ok(StartResult { info, session })
}

#[tauri::command]
pub fn engine_send(state: State<'_, EngineState>, line: String) -> Result<(), String> {
    let mut slot = state.process.lock().unwrap();
    let Some(process) = slot.as_mut() else {
        return Err("Engine is not running.".into());
    };
    let Some(stdin) = process.stdin.as_mut() else {
        return Err("Engine stdin is closed.".into());
    };
    if std::env::var_os("NIMBUS_TRACE").is_some() {
        // `options.set` carries the password in a JSON `value`, not in the
        // `password="..."` form the redactor handles.
        let safe = if line.to_ascii_lowercase().contains("\"password\"") {
            "[credential update redacted]".to_string()
        } else {
            redact(&line)
        };
        eprintln!("[engine-in] {safe}");
    }
    stdin
        .write_all(format!("{line}\n").as_bytes())
        .and_then(|_| stdin.flush())
        .map_err(|e| format!("Could not write to engine: {e}"))
}

#[tauri::command]
pub fn engine_stop(state: State<'_, EngineState>) {
    let mut slot = state.process.lock().unwrap();
    stop_locked(&mut slot);
}

/// TCP round-trip estimate for the server list. We connect to 443 because every
/// AirVPN entry point is reachable there; a refused connection still measures
/// the network round trip.
#[tauri::command]
pub async fn servers_latency(hosts: Vec<String>) -> Vec<Option<u32>> {
    tauri::async_runtime::spawn_blocking(move || probe_latency(hosts))
        .await
        .unwrap_or_default()
}

fn probe_one(host: &str) -> Option<u32> {
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::{Duration, Instant};

    let addrs: Vec<std::net::SocketAddr> = format!("{host}:443").to_socket_addrs().ok()?.collect();
    let target = *addrs.first()?;
    let started = Instant::now();
    let result = TcpStream::connect_timeout(&target, Duration::from_millis(1400));
    let elapsed = started.elapsed().as_millis() as u32;
    match result {
        Ok(_) => Some(elapsed),
        Err(_) if elapsed < 1350 => Some(elapsed),
        Err(_) => None,
    }
}

fn probe_latency(hosts: Vec<String>) -> Vec<Option<u32>> {
    let total = hosts.len();
    let mut out = vec![None; total];
    if total == 0 {
        return out;
    }
    let chunk = total.div_ceil(24);

    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for (index, slice) in hosts.chunks(chunk).enumerate() {
            let offset = index * chunk;
            handles.push(scope.spawn(move || {
                slice
                    .iter()
                    .enumerate()
                    .map(|(i, host)| (offset + i, probe_one(host)))
                    .collect::<Vec<_>>()
            }));
        }
        for handle in handles {
            if let Ok(rows) = handle.join() {
                for (index, value) in rows {
                    if index < total {
                        out[index] = value;
                    }
                }
            }
        }
    });

    out
}

#[cfg(test)]
mod tests {
    use super::{redact, redact_json_password};

    #[test]
    fn redacts_raw_and_escaped_passwords() {
        assert_eq!(redact(r#"password="secret""#), r#"password="***""#);
        assert_eq!(redact(r#"password=\"secret\""#), r#"password="***""#);
        assert_eq!(redact(r#"login="user" password=\"secret\" connect="true""#),
                   r#"login="user" password="***" connect="true""#);
    }

    #[test]
    fn redacts_json_option_changes() {
        assert_eq!(
            redact_json_password(
                r#"{"command":"option.change","name":"password","value":"s3cr3t"}"#
            ),
            r#"{"command":"option.change","name":"password","value":"***"}"#
        );
        assert_eq!(
            redact_json_password(
                r#"{"command":"option.change","name":"password","value":"quote\"inside"}"#
            ),
            r#"{"command":"option.change","name":"password","value":"***"}"#
        );
        assert_eq!(
            redact_json_password(r#"{"command":"option.change","name":"login","value":"user"}"#),
            r#"{"command":"option.change","name":"login","value":"user"}"#
        );
    }
}
