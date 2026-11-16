//! Apply and remove an app-based split tunneling session.
//!
//! One `Session` owns everything a single VPN connection needs:
//!
//! * the `nimbus.apps` cgroup and a watcher that keeps selected apps in it;
//! * the nftables mark that tags their packets;
//! * the policy rule and table [`plan::TABLE`] that send those packets into
//!   the tunnel.
//!
//! [`Session::begin`] is idempotent: it first clears whatever a crashed earlier
//! run left behind, so a stale mark or route can never outlive the session that
//! installed it.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::{apps, exec, plan};

/// How often the watcher re-scans `/proc` for newly started instances of the
/// selected apps. Half a second is the window in which a freshly launched
/// browser could send its first packets outside the tunnel.
const WATCH_INTERVAL: Duration = Duration::from_millis(500);

pub struct Session {
    pub tunnel: String,
    stop: Arc<AtomicBool>,
    watcher: Option<thread::JoinHandle<()>>,
}

pub fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

pub fn require_root() -> Result<(), String> {
    if is_root() {
        Ok(())
    } else {
        Err("this command must run as root (try: sudo nimbus-helper …)".into())
    }
}

/// Find the tunnel device Eddie is using.
///
/// Eddie names its WireGuard interface `Eddie`; OpenVPN gets a `tunN`. Never
/// guess from a config file: the device only exists while connected.
pub fn detect_tunnel() -> Option<String> {
    let entries = fs::read_dir("/sys/class/net").ok()?;
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let state = fs::read_to_string(format!("/sys/class/net/{name}/operstate"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        // Point-to-point tunnel devices report `unknown` rather than `up`.
        let live = state == "up" || state == "unknown";
        names.push((name, live));
    }

    if names.iter().any(|(n, live)| n == "Eddie" && *live) {
        return Some("Eddie".into());
    }
    names
        .into_iter()
        .find(|(n, live)| {
            *live
                && (n.starts_with("wg") || n.starts_with("tun") || n.starts_with("tap"))
                && n.chars().any(|c| c.is_ascii_digit())
        })
        .map(|(n, _)| n)
}

fn cgroup_procs() -> std::path::PathBuf {
    Path::new(plan::CGROUP_PATH).join("cgroup.procs")
}

fn adopted_pids() -> HashSet<i32> {
    fs::read_to_string(cgroup_procs())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse::<i32>().ok())
        .collect()
}

/// Move every PID whose executable matches `apps` into the cgroup.
///
/// `owner` is the desktop user the helper was invoked for. A PID is adopted only
/// when its owner matches, so a root-owned process can never be steered into a
/// user's tunnel. `None` means "no filter" and is reserved for an explicit
/// `--allow-any-uid`.
///
/// Returns how many processes were moved.
pub fn adopt_matching(apps: &[String], owner: Option<u32>) -> Result<usize, String> {
    if apps.is_empty() {
        return Ok(0);
    }
    let already = adopted_pids();
    let mut buffer = String::new();
    let mut moved = 0;
    for pid in apps::matching_pids(apps) {
        if already.contains(&pid) {
            continue;
        }
        if let Some(owner) = owner
            && apps::pid_owner(pid) != Some(owner)
        {
            continue;
        }
        buffer.push_str(&format!("{pid}\n"));
        moved += 1;
    }
    if moved == 0 {
        return Ok(0);
    }
    // One `write()` per PID. `cgroup.procs` accepts exactly one PID per write;
    // a buffer holding several returns EINVAL and moves nothing. Measured on
    // kernel 7.3.4: 27 PIDs in one write failed, the same 27 one at a time all
    // succeeded.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .open(cgroup_procs())
        .map_err(|e| format!("could not open {}: {e}", cgroup_procs().display()))?;
    let mut adopted = 0;
    let mut failures: Vec<(i32, std::io::Error)> = Vec::new();
    for pid in buffer.lines() {
        match file.write_all(format!("{pid}\n").as_bytes()) {
            Ok(()) => adopted += 1,
            // The process exited between the /proc scan and this write, which
            // is ordinary and not an error.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => failures.push((pid.parse().unwrap_or(-1), e)),
        }
    }
    // A process we cannot move stays outside the VPN while the user believes it
    // is inside. Fail loudly instead of silently under-enforcing.
    if let Some((pid, error)) = failures.first() {
        let more = if failures.len() > 1 {
            format!(" (and {} more)", failures.len() - 1)
        } else {
            String::new()
        };
        return Err(format!(
            "could not move process {pid} into the cgroup: {error}{more}"
        ));
    }
    Ok(adopted)
}

/// Move every process back to the root cgroup so the directory can be removed.
fn release_all() {
    let pids = adopted_pids();
    if !pids.is_empty()
        && let Ok(mut file) = fs::OpenOptions::new()
            .write(true)
            .open(plan::CGROUP_ROOT_PROCS)
    {
        // One write per PID: a process may have exited, and a single failed
        // line would abort the rest of a batched write.
        for pid in pids {
            let _ = file.write_all(format!("{pid}\n").as_bytes());
        }
    }
    let _ = fs::remove_dir(plan::CGROUP_PATH);
}

fn ensure_cgroup() -> Result<(), String> {
    fs::create_dir_all(plan::CGROUP_PATH)
        .map_err(|e| format!("could not create {}: {e}", plan::CGROUP_PATH))?;
    if !cgroup_procs().exists() {
        return Err(format!(
            "{} exists but has no cgroup.procs — is cgroup v2 mounted at /sys/fs/cgroup?",
            plan::CGROUP_PATH
        ));
    }
    Ok(())
}

/// Remove state left by an earlier session that did not tear down cleanly.
fn reset_state() {
    exec::remove_rule("iptables", &plan::mark_rule_delete());
    exec::remove_rule("ip6tables", &plan::v6_drop_rule_delete());
    exec::remove_rule("iptables", &plan::masquerade_rule_delete());
    exec::remove_rule("iptables", &plan::masquerade_rule_delete());
    exec::run_ignore("ip", &plan::table_flush());
    exec::run_ignore("ip", &plan::rule_del());
    release_all();
}

/// Routes, policy rule, mark, then adoption. Split out of [`Session::begin`] so
/// a failure anywhere in the sequence can undo the whole thing.
fn install(tunnel: &str, app_ids: &[String], owner: Option<u32>) -> Result<(), String> {
    // Routes first: the policy rule is only installed once table 100 has
    // something in it, so a marked packet is never looked up in an empty table.
    let main_routes = exec::ip(&["-4".into(), "route".into(), "show".into()])?;
    for route in plan::copied_routes(tunnel, &main_routes) {
        exec::ip(&route)?;
    }
    exec::ip(&plan::default_route(tunnel))?;
    exec::ip(&plan::rule_add())?;

    // Only now start marking, and with iptables: its MARK target reroutes the
    // packet, while an nftables `meta mark set` in OUTPUT runs after the route
    // lookup and would let the packet leave unsteered.
    exec::ensure_rule("iptables", &plan::mark_rule(), &plan::mark_rule_check())?;
    exec::ensure_rule(
        "ip6tables",
        &plan::v6_drop_rule(),
        &plan::v6_drop_rule_check(),
    )?;

    // The kernel picked each socket's source address during the route lookup
    // that happened before this mark existed, so it is still the LAN address.
    // Rewrite it to the tunnel address on the way out, or the VPN exit drops
    // every packet from an included app.
    exec::ensure_rule(
        "iptables",
        &plan::masquerade_rule(),
        &plan::masquerade_rule_check(),
    )?;

    adopt_matching(app_ids, owner)?;
    Ok(())
}

impl Session {
    /// Whether the tunnel device is still there. The daemon polls this so a
    /// dropped VPN cannot leave a mark pointing at a device that has gone.
    pub fn tunnel_present(&self) -> bool {
        Path::new(&format!("/sys/class/net/{}", self.tunnel)).exists()
    }

    /// Install the whole policy for `apps` over `tunnel`.
    ///
    /// `owner` is the uid whose processes may be adopted (see
    /// [`adopt_matching`]).
    pub fn begin(tunnel: &str, app_ids: &[String], owner: Option<u32>) -> Result<Self, String> {
        require_root()?;
        if app_ids.is_empty() {
            return Err("no apps selected".into());
        }
        if !Path::new(&format!("/sys/class/net/{tunnel}")).exists() {
            return Err(format!(
                "tunnel device {tunnel} is not present — connect the VPN first"
            ));
        }

        reset_state();
        ensure_cgroup()?;

        // Everything below installs state, so any failure has to undo it: a
        // mark and a policy rule left behind would silently steer packets of
        // whatever later enters the cgroup.
        if let Err(error) = install(tunnel, app_ids, owner) {
            reset_state();
            return Err(error);
        }

        let stop = Arc::new(AtomicBool::new(false));
        let watcher = spawn_watcher(stop.clone(), app_ids.to_vec(), owner);

        Ok(Self {
            tunnel: tunnel.to_string(),
            stop,
            watcher: Some(watcher),
        })
    }

    /// Remove the mark, then the rule, then the table, then the cgroup.
    pub fn teardown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.watcher.take() {
            let _ = handle.join();
        }
        // Order matters: stop marking before the table the mark points at
        // disappears, otherwise selected packets would be looked up in a
        // table that no longer holds a route.
        exec::remove_rule("iptables", &plan::mark_rule_delete());
        exec::remove_rule("ip6tables", &plan::v6_drop_rule_delete());
        exec::run_ignore("ip", &plan::table_flush());
        exec::run_ignore("ip", &plan::rule_del());
        release_all();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.watcher.is_some() {
            self.teardown();
        }
    }
}

fn spawn_watcher(
    stop: Arc<AtomicBool>,
    apps: Vec<String>,
    owner: Option<u32>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            thread::sleep(WATCH_INTERVAL);
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if !Path::new(plan::CGROUP_PATH).exists() {
                break; // torn down underneath us
            }
            match adopt_matching(&apps, owner) {
                Ok(0) => {}
                Ok(n) => eprintln!("nimbus-helper: adopted {n} new process(es)"),
                Err(e) => eprintln!("nimbus-helper: watcher: {e}"),
            }
        }
    })
}

/// Write a PID into a `cgroup.procs` file, moving that process.
fn move_pid(procs: &str, pid: u32) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .open(procs)
        .map_err(|e| format!("could not open {procs}: {e}"))?;
    file.write_all(format!("{pid}\n").as_bytes())
        .map_err(|e| format!("could not write {pid} to {procs}: {e}"))
}

/// Public egress address as seen from this process.
fn curl_egress() -> Result<String, String> {
    let output = std::process::Command::new("curl")
        .args(["-s", "--max-time", "8", "https://api.ipify.org"])
        .output()
        .map_err(|e| format!("could not run curl: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("curl failed: {}", err.trim()));
    }
    let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if ip.is_empty() {
        return Err("curl returned no address".into());
    }
    Ok(ip)
}

impl Session {
    /// Prove the policy with real packets.
    ///
    /// The helper moves *itself* into the cgroup, so the `curl` it spawns
    /// inherits membership without a race, and asks a public service which
    /// address it is seen from — once before joining, once after. The two
    /// answers differ only if the mark, the policy rule, and table 100 are all
    /// working together.
    pub fn self_test(&self) -> Result<(String, String), String> {
        let outside = curl_egress()?;
        move_pid(cgroup_procs().to_string_lossy().as_ref(), std::process::id())?;
        let inside = curl_egress();
        // Always leave the cgroup again, even when the probe failed.
        let _ = move_pid(plan::CGROUP_ROOT_PROCS, std::process::id());
        Ok((outside, inside?))
    }
}

/// Packets that the firewall has marked since the rule was installed.
fn marked_packets() -> u64 {
    let listing = exec::run(
        "iptables",
        &["-t".into(), "mangle".into(), "-L".into(), "OUTPUT".into(), "-v".into(), "-x".into(), "-n".into()],
    )
    .unwrap_or_default();
    listing
        .lines()
        .filter(|line| line.contains("MARK"))
        .filter_map(|line| line.split_whitespace().next())
        .filter_map(|count| count.parse::<u64>().ok())
        .sum()
}

fn mark_rule_present() -> bool {
    exec::run("iptables", &plan::mark_rule_check()).is_ok()
}

/// Snapshot of the installed state, for `nimbus-helper status` and debugging.
#[derive(Debug, serde::Serialize)]
pub struct Status {
    pub root: bool,
    pub tunnel: Option<String>,
    pub cgroup: bool,
    pub adopted: usize,
    pub mark_rule: bool,
    pub marked: u64,
    pub rule: bool,
    pub table_routes: Vec<String>,
}

pub fn status() -> Status {
    let rules = exec::ip(&["rule".into(), "show".into()]).unwrap_or_default();
    let table = exec::ip(&[
        "route".into(),
        "show".into(),
        "table".into(),
        plan::TABLE.to_string(),
    ])
    .unwrap_or_default();

    Status {
        root: is_root(),
        tunnel: detect_tunnel(),
        cgroup: Path::new(plan::CGROUP_PATH).exists(),
        adopted: adopted_pids().len(),
        mark_rule: mark_rule_present(),
        marked: marked_packets(),
        rule: rules.contains(&format!("0x{mark:x}", mark = plan::MARK)),
        table_routes: table.lines().map(str::to_string).collect(),
    }
}
