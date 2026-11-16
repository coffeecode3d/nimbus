//! `nimbus-helper` — privileged half of app-based split tunneling.
//!
//! Nimbus itself never runs as root. It starts this helper once per VPN
//! session; the helper installs the cgroup, the nftables mark, and the policy
//! route, then stays in the foreground until it is told to stop (or its parent
//! dies), and removes everything on the way out.
//!
//! Commands:
//!
//! ```text
//! nimbus-helper apps  [--json] [--all]      list running applications
//! nimbus-helper plan  [--apps a,b]          print the policy, touch nothing
//! nimbus-helper enforce --apps a,b [--tunnel Eddie] [--dry-run] [--parent-death]
//! nimbus-helper status [--json]             what is installed right now
//! ```

use nimbus_helper::{apps, ipc, plan, session, signal};

use std::thread;
use std::time::Duration;

fn main() {
    signal::reset_sigpipe();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(&args) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("nimbus-helper: {err}");
            2
        }
    };
    std::process::exit(code);
}

fn run(args: &[String]) -> Result<i32, String> {
    let command = args.first().map(String::as_str).unwrap_or("help");
    let rest = args.get(1..).unwrap_or_default();
    match command {
        "apps" => cmd_apps(rest),
        "plan" => cmd_plan(rest),
        "enforce" => cmd_enforce(rest),
        "status" => cmd_status(rest),
        "serve" => cmd_serve(rest),
        "version" | "--version" => {
            println!("nimbus-helper {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        "help" | "--help" | "-h" => {
            usage();
            Ok(0)
        }
        other => Err(format!("unknown command '{other}' (try: nimbus-helper help)")),
    }
}

fn usage() {
    println!(
        "\
nimbus-helper {version}

  apps   [--json] [--all]                  list running applications
  plan   [--apps a,b] [--tunnel NAME]      print the policy without applying it
  enforce --apps a,b [--tunnel NAME]       apply it until SIGTERM/stdin EOF
          [--dry-run] [--parent-death] [--self-test] [--allow-any-uid]
  status [--json]                          show what is installed right now
  serve  [--socket PATH]                   run the root daemon Nimbus talks to

Only processes owned by the invoking user (SUDO_UID/PKEXEC_UID) are ever moved
into the tunnel, so a root daemon can never be steered into a user's session.

`--parent-death` asks Linux to signal the helper when Nimbus exits, and watches
stdin for EOF, so a killed UI cannot leave a mark or route behind.",
        version = env!("CARGO_PKG_VERSION")
    );
}

// ── argument helpers ───────────────────────────────────────────────────────

fn flag(rest: &[String], name: &str) -> Option<String> {
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix(&format!("--{name}=")) {
            return Some(value.to_string());
        }
        if arg == &format!("--{name}") {
            return iter.next().cloned();
        }
    }
    None
}

fn has(rest: &[String], name: &str) -> bool {
    rest.iter().any(|a| a == &format!("--{name}"))
}

/// `--apps a,b` plus repeated `--app a`.
fn app_ids(rest: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(value) = flag(rest, "apps") {
        out.extend(
            value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        );
    }
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        let value = if let Some(v) = arg.strip_prefix("--app=") {
            Some(v.to_string())
        } else if arg == "--app" {
            iter.next().cloned()
        } else {
            None
        };
        if let Some(value) = value {
            let value = value.trim().to_string();
            if !value.is_empty() && !out.contains(&value) {
                out.push(value);
            }
        }
    }
    out.dedup();
    out
}

/// The desktop user's uid, so `apps` does not list root daemons when the helper
/// happens to run under sudo, and so `enforce` only ever adopts that user's
/// processes.
fn invoking_uid() -> Option<u32> {
    for key in ["SUDO_UID", "PKEXEC_UID"] {
        if let Ok(value) = std::env::var(key)
            && let Ok(uid) = value.trim().parse::<u32>()
        {
            return Some(uid);
        }
    }
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    if uid == 0 { None } else { Some(uid) }
}

// ── commands ───────────────────────────────────────────────────────────────

fn cmd_apps(rest: &[String]) -> Result<i32, String> {
    let mut list = apps::list();
    if !has(rest, "all")
        && let Some(uid) = invoking_uid()
    {
        list.retain(|app| app.uids.contains(&uid));
    }

    if has(rest, "json") {
        let json =
            serde_json::to_string_pretty(&list).map_err(|e| format!("could not encode apps: {e}"))?;
        println!("{json}");
    } else {
        println!("{:<30} {:<34} {:>5}", "ID", "NAME", "PIDS");
        for app in &list {
            println!("{:<30} {:<34} {:>5}", app.id, app.name, app.pids.len());
        }
        println!("\n{} application(s)", list.len());
    }
    Ok(0)
}

fn cmd_plan(rest: &[String]) -> Result<i32, String> {
    let ids = app_ids(rest);
    let tunnel = flag(rest, "tunnel").or_else(session::detect_tunnel);

    println!("# commands, in order");
    match &tunnel {
        Some(tunnel) => {
            println!("ip {}", plan::default_route(tunnel).join(" "));
            println!(
                "ip route add <tunnel subnet> dev {tunnel} table {table}   # if the tunnel has one",
                table = plan::TABLE
            );
        }
        None => println!(
            "# no tunnel device detected — connect the VPN, or pass --tunnel NAME"
        ),
    }
    println!("ip {}", plan::rule_add().join(" "));
    println!("iptables {}", plan::mark_rule().join(" "));
    println!("ip6tables {}", plan::v6_drop_rule().join(" "));
    if ids.is_empty() {
        println!("# nimbus-helper enforce --apps <a,b>");
    } else {
        println!("# adopt processes whose executable is: {}", ids.join(", "));
    }

    println!("\n# teardown");
    println!("iptables {}", plan::mark_rule_delete().join(" "));
    println!("ip6tables {}", plan::v6_drop_rule_delete().join(" "));
    for step in plan::teardown_steps() {
        println!("ip {}", step.join(" "));
    }

    println!(
        "\n# the cgroup {cgroup} must exist before the iptables rules are installed",
        cgroup = plan::CGROUP_PATH
    );
    Ok(0)
}

fn cmd_enforce(rest: &[String]) -> Result<i32, String> {
    let ids = app_ids(rest);
    if ids.is_empty() {
        return Err("no apps given (use --apps a,b or --app NAME)".into());
    }
    let tunnel = flag(rest, "tunnel")
        .or_else(session::detect_tunnel)
        .ok_or_else(|| {
            "no tunnel device detected — connect the VPN first, or pass --tunnel NAME".to_string()
        })?;

    if has(rest, "dry-run") {
        println!("# would apply to: {}", ids.join(", "));
        println!("# tunnel: {tunnel}");
        println!("ip {}", plan::default_route(&tunnel).join(" "));
        println!("ip {}", plan::rule_add().join(" "));
        println!("iptables {}", plan::mark_rule().join(" "));
        println!("ip6tables {}", plan::v6_drop_rule().join(" "));
        return Ok(0);
    }

    session::require_root()?;

    // Refuse to guess: adopting another user's processes would let a caller
    // steer root traffic into a tunnel they control.
    let owner = if has(rest, "allow-any-uid") {
        None
    } else {
        match invoking_uid() {
            Some(uid) => Some(uid),
            None => {
                return Err(
                    "refusing to adopt processes: the invoking user is unknown.\n\
                     Run through sudo/pkexec (which set SUDO_UID/PKEXEC_UID), or pass\n\
                     --allow-any-uid if this really is a dedicated root session."
                        .into(),
                );
            }
        }
    };

    if has(rest, "parent-death") {
        // SAFETY: PR_SET_PDEATHSIG only stores a signal number on this process.
        unsafe {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
        }
        spawn_stdin_watch();
    }
    signal::install();

    let mut session = session::Session::begin(&tunnel, &ids, owner)?;
    println!(
        "nimbus-helper: VPN applies to {} via {}",
        ids.join(", "),
        session.tunnel
    );

    if has(rest, "self-test") {
        match session.self_test() {
            Ok((outside, inside)) => {
                println!("  outside the tunnel : {outside}");
                println!("  inside the tunnel  : {inside}");
                if outside == inside {
                    eprintln!(
                        "  FAIL: both probes report the same address — the policy is not steering traffic"
                    );
                } else {
                    println!("  OK: selected processes now egress through the VPN");
                }
            }
            Err(e) => eprintln!("  self-test could not run: {e}"),
        }
    }

    while !signal::stopping() {
        thread::sleep(Duration::from_millis(200));
    }

    eprintln!("nimbus-helper: stopping");
    session.teardown();
    Ok(0)
}

fn cmd_serve(rest: &[String]) -> Result<i32, String> {
    session::require_root()?;
    let socket = flag(rest, "socket").unwrap_or_else(|| ipc::DEFAULT_SOCKET.to_string());
    signal::install();
    if let Err(e) = ipc::serve(&socket) {
        return Err(format!("daemon stopped: {e}"));
    }
    Ok(0)
}

fn cmd_status(rest: &[String]) -> Result<i32, String> {
    let status = session::status();
    if has(rest, "json") {
        let json = serde_json::to_string_pretty(&status)
            .map_err(|e| format!("could not encode status: {e}"))?;
        println!("{json}");
        return Ok(0);
    }

    println!("root            {}", status.root);
    println!(
        "tunnel          {}",
        status.tunnel.as_deref().unwrap_or("(none detected)")
    );
    println!("cgroup          {}", status.cgroup);
    println!("adopted pids    {}", status.adopted);
    println!("mark rule       {}", status.mark_rule);
    println!("marked packets  {}", status.marked);
    println!("policy rule     {}", status.rule);
    if status.table_routes.is_empty() {
        println!("table {}         (empty)", plan::TABLE);
    } else {
        println!("table {}", plan::TABLE);
        for route in &status.table_routes {
            println!("  {route}");
        }
    }
    Ok(0)
}

// ── lifecycle plumbing ─────────────────────────────────────────────────────

/// Tear down when the parent closes our stdin, which is how Nimbus signals
/// "the UI is gone" without depending on a signal being delivered.
fn spawn_stdin_watch() {
    thread::spawn(|| {
        use std::io::Read;
        let mut stdin = std::io::stdin();
        let mut buffer = [0u8; 256];
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) | Err(_) => {
                    signal::request_stop();
                    break;
                }
                Ok(_) => {}
            }
        }
    });
}
