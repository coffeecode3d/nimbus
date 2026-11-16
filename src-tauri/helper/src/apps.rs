//! Enumerate running applications.
//!
//! The picker shows one row per executable, not one row per process: Helium
//! usually runs a dozen PIDs and the user thinks of that as one app. The
//! executable basename is the stable identifier that the cgroup watcher matches
//! on, so it is what the UI stores and what `--apps` accepts.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, serde::Serialize)]
pub struct App {
    /// Executable basename, e.g. `helium-browser`. Stable across restarts.
    pub id: String,
    /// Human name from the desktop entry, falling back to the id.
    pub name: String,
    /// Icon name from the desktop entry, resolved by the frontend.
    pub icon: Option<String>,
    /// Full path of the executable, as seen for the first process.
    pub exe_path: String,
    /// Live PIDs belonging to this executable.
    pub pids: Vec<i32>,
    /// Owners of those PIDs. The helper only adopts processes it is allowed to.
    pub uids: Vec<u32>,
    /// True when a desktop entry describes a GUI application.
    pub gui: bool,
}

#[derive(Default)]
struct DesktopEntry {
    name: Option<String>,
    icon: Option<String>,
    gui: bool,
}

/// Running applications, one entry per executable, sorted by display name.
pub fn list() -> Vec<App> {
    let desktop = desktop_index();
    let mut by_id: BTreeMap<String, App> = BTreeMap::new();

    let Ok(dir) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    for entry in dir.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        // PID 1 is systemd; it owns no GUI and must never be offered.
        if pid <= 1 {
            continue;
        }
        let proc_dir = PathBuf::from("/proc").join(pid.to_string());
        let Ok(link) = fs::read_link(proc_dir.join("exe")) else {
            continue; // kernel thread, or a process we may not inspect
        };
        let exe = strip_deleted(&link);
        let Some(id) = exe.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        if id.is_empty() {
            continue;
        }
        let exe_path = exe.display().to_string();
        let uid = fs::metadata(&proc_dir).map(|m| m.uid()).unwrap_or(u32::MAX);

        let app = by_id.entry(id.clone()).or_insert_with(|| {
            let meta = desktop.get(&id);
            App {
                id: id.clone(),
                name: meta
                    .and_then(|m| m.name.clone())
                    .unwrap_or_else(|| id.clone()),
                icon: meta.and_then(|m| m.icon.clone()),
                exe_path: exe_path.clone(),
                pids: Vec::new(),
                uids: Vec::new(),
                gui: meta.map(|m| m.gui).unwrap_or(false),
            }
        });
        if !app.pids.contains(&pid) {
            app.pids.push(pid);
        }
        if !app.uids.contains(&uid) {
            app.uids.push(uid);
        }
    }

    let mut apps: Vec<App> = by_id.into_values().collect();
    for app in &mut apps {
        app.pids.sort_unstable();
        app.uids.sort_unstable();
    }
    apps.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    apps
}

/// Running applications filtered to one owner, or all of them when `uid` is
/// `None`.
pub fn list_for(uid: Option<u32>) -> Vec<App> {
    let mut list = list();
    if let Some(uid) = uid {
        list.retain(|app| app.uids.contains(&uid));
    }
    list
}

/// PIDs whose executable matches one of `apps`.
///
/// Matching is case-insensitive on the basename, or exact on the full path, so
/// the UI can store either. This is also what the cgroup watcher calls, which
/// is why a relaunched app is picked up again.
pub fn matching_pids(apps: &[String]) -> Vec<i32> {
    if apps.is_empty() {
        return Vec::new();
    }
    let wanted: Vec<String> = apps.iter().map(|a| a.to_lowercase()).collect();
    let mut out = Vec::new();

    let Ok(dir) = fs::read_dir("/proc") else {
        return out;
    };
    for entry in dir.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        if pid <= 1 {
            continue;
        }
        let Ok(link) = fs::read_link(PathBuf::from("/proc").join(pid.to_string()).join("exe"))
        else {
            continue;
        };
        let exe = strip_deleted(&link);
        let full = exe.to_string_lossy().to_lowercase();
        let base = exe
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if wanted.iter().any(|w| *w == base || *w == full) {
            out.push(pid);
        }
    }
    out.sort_unstable();
    out
}

/// Owner of a process, or `None` when `/proc/<pid>` is already gone.
pub fn pid_owner(pid: i32) -> Option<u32> {
    fs::metadata(format!("/proc/{pid}"))
        .ok()
        .map(|m| m.uid())
}

/// `/proc/<pid>/exe` points at `/path/to/binary (deleted)` once the binary on
/// disk has been replaced, which is common after a browser update.
fn strip_deleted(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_suffix(" (deleted)") {
        Some(trimmed) => PathBuf::from(trimmed),
        None => path.to_path_buf(),
    }
}

fn desktop_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
    ];
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(data).join("applications"));
    } else if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }
    if let Some(data_dirs) = std::env::var_os("XDG_DATA_DIRS") {
        for dir in std::env::split_paths(&data_dirs) {
            dirs.push(dir.join("applications"));
        }
    }
    dirs
}

/// Map executable basename → desktop entry, so the picker can show "Helium"
/// with its icon instead of `helium-browser`.
fn desktop_index() -> HashMap<String, DesktopEntry> {
    let mut index: HashMap<String, DesktopEntry> = HashMap::new();
    for dir in desktop_dirs() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let Some((exe, meta)) = parse_desktop(&text) else {
                continue;
            };
            // Earlier directories win, matching XDG precedence.
            index.entry(exe).or_insert(meta);
        }
    }
    index
}

/// Parse the `[Desktop Entry]` group of a `.desktop` file.
fn parse_desktop(text: &str) -> Option<(String, DesktopEntry)> {
    let mut in_entry = false;
    let mut seen_entry = false;
    let mut name = None;
    let mut icon = None;
    let mut exec = None;
    let mut terminal = false;
    let mut hidden = false;
    let mut kind = None;

    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            if seen_entry {
                break; // only the first [Desktop Entry] group counts
            }
            in_entry = line == "[Desktop Entry]";
            seen_entry |= in_entry;
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Name" => name = Some(value.to_string()),
            "Icon" => icon = Some(value.to_string()),
            "Exec" => exec = Some(value.to_string()),
            "Terminal" => terminal = value.eq_ignore_ascii_case("true"),
            "NoDisplay" => hidden = value.eq_ignore_ascii_case("true"),
            "Type" => kind = Some(value.to_string()),
            _ => {}
        }
    }

    // A NoDisplay entry still names a running app correctly, so it is kept.
    let _ = hidden;
    let exec = exec?;
    let exe = exec_basename(&exec)?;
    Some((
        exe,
        DesktopEntry {
            name,
            icon,
            gui: kind.as_deref() == Some("Application") && !terminal,
        },
    ))
}

/// First token of `Exec=`, minus wrappers and field codes, reduced to its
/// basename. `Exec=env FOO=1 /usr/bin/foo %U` yields `foo`.
fn exec_basename(exec: &str) -> Option<String> {
    let tokens = tokenize_exec(exec);
    let mut index = 0;
    while index < tokens.len() && (tokens[index] == "env" || is_assignment(&tokens[index])) {
        index += 1;
    }
    let token = tokens.get(index)?;
    let base = Path::new(token).file_name()?.to_str()?.to_string();
    if base.is_empty() || base.starts_with('%') {
        None
    } else {
        Some(base)
    }
}

/// Split an `Exec=` value on whitespace, but keep quoted paths (which may
/// contain spaces) in one piece.
fn tokenize_exec(exec: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for c in exec.trim().chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// `KEY=value` in front of the program, as `env` accepts after `env`.
fn is_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((key, _)) => !key.is_empty() && !key.contains('/'),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_current_process() {
        let apps = list();
        let me = std::process::id() as i32;
        let found = apps.iter().any(|a| a.pids.contains(&me));
        assert!(found, "current pid {me} missing from {} apps", apps.len());
    }

    #[test]
    fn matching_pids_finds_the_test_binary() {
        let me = std::process::id() as i32;
        let exe = fs::read_link(format!("/proc/{me}/exe")).expect("read own exe");
        let base = exe.file_name().unwrap().to_string_lossy().to_string();
        let pids = matching_pids(&[base.clone()]);
        assert!(pids.contains(&me), "{base} not matched in {pids:?}");
        assert!(matching_pids(&["definitely-not-running".into()]).is_empty());
        assert!(matching_pids(&[]).is_empty());
    }

    #[test]
    fn current_process_is_owned_by_us() {
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        assert_eq!(pid_owner(std::process::id() as i32), Some(uid));
        assert_eq!(pid_owner(-1), None);
    }

    #[test]
    fn parses_a_desktop_entry() {
        let text = "\
[Desktop Entry]
Type=Application
Name=Helium
Icon=helium
Exec=/usr/bin/helium-browser %U
Terminal=false
";
        let (exe, meta) = parse_desktop(text).expect("parsed");
        assert_eq!(exe, "helium-browser");
        assert_eq!(meta.name.as_deref(), Some("Helium"));
        assert_eq!(meta.icon.as_deref(), Some("helium"));
        assert!(meta.gui);
    }

    #[test]
    fn env_wrappers_and_field_codes_are_stripped() {
        assert_eq!(
            exec_basename("env BAMF_DESKTOP_FILE_HINT=/x /usr/bin/foo %U").as_deref(),
            Some("foo")
        );
        assert_eq!(exec_basename("sh -c 'x'").as_deref(), Some("sh"));
        assert_eq!(
            exec_basename("\"/opt/Some App/bin/app\" --flag").as_deref(),
            Some("app")
        );
        assert_eq!(exec_basename("%U").as_deref(), None);
    }
}
