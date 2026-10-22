use crate::engine::{self, EngineInfo};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// AirVPN's Eddie release-signing key. Pinning the key is what makes the
/// download trustworthy even if the download helper or HTTPS endpoint were
/// compromised: only an archive signed by this key is installed.
const EDDIE_KEY: &str = include_str!("../resources/eddie_maintainer_gpg.asc");
const EDDIE_KEY_FINGERPRINT: &str = "239FD9507903EA9FBF7DCCF1C6E338858A239AEC";
const INSTALL_DIR: &str = "/opt/eddie-ui";

static INSTALLING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Deserialize)]
struct DownloadInfo {
    filename: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct ChecksumInfo {
    checksum: String,
}

#[derive(Debug, Deserialize)]
struct SignatureInfo {
    signature_url: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallEvent {
    stage: String,
    message: String,
    progress: Option<u8>,
}

fn emit(app: &AppHandle, stage: &str, message: impl Into<String>, progress: Option<u8>) {
    let _ = app.emit(
        "engine://install",
        InstallEvent {
            stage: stage.to_string(),
            message: message.into(),
            progress,
        },
    );
}

struct InstallGuard;

impl Drop for InstallGuard {
    fn drop(&mut self) {
        INSTALLING.store(false, Ordering::SeqCst);
    }
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn cache_root() -> PathBuf {
    if let Some(value) = std::env::var_os("XDG_CACHE_HOME") {
        if !value.is_empty() {
            return PathBuf::from(value);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".cache");
    }
    std::env::temp_dir()
}

#[cfg(unix)]
fn private_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(path).map_err(|e| format!("Could not create {}: {e}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("Could not secure {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|e| format!("Could not create {}: {e}", path.display()))
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("Nimbus/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| format!("Could not start the download client: {e}"))
}

fn get_json<T: for<'de> Deserialize<'de>>(
    client: &reqwest::blocking::Client,
    url: &str,
) -> Result<T, String> {
    let response = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|e| format!("AirVPN's download service did not respond: {e}"))?;
    response
        .json::<T>()
        .map_err(|e| format!("AirVPN's download service returned an unexpected response: {e}"))
}

fn ensure_official(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| format!("AirVPN returned an invalid download address: {e}"))?;
    if parsed.scheme() != "https" || parsed.host_str() != Some("eddie.website") {
        return Err("AirVPN returned a download address outside eddie.website.".into());
    }
    Ok(())
}

fn download(
    app: &AppHandle,
    client: &reqwest::blocking::Client,
    url: &str,
    destination: &Path,
    progress: bool,
) -> Result<(), String> {
    ensure_official(url)?;
    let mut response = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|e| format!("Download failed: {e}"))?;
    if response.url().scheme() != "https" || response.url().host_str() != Some("eddie.website") {
        return Err("Download redirected outside eddie.website.".into());
    }

    let total = response.content_length().unwrap_or(0);
    let mut file = File::create(destination)
        .map_err(|e| format!("Could not create {}: {e}", destination.display()))?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut received = 0_u64;
    let mut last = Instant::now();

    loop {
        let read = response
            .read(&mut buffer)
            .map_err(|e| format!("Download interrupted: {e}"))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(|e| format!("Could not write the download: {e}"))?;
        received += read as u64;
        if progress && last.elapsed() >= Duration::from_millis(150) {
            last = Instant::now();
            let percent = if total > 0 {
                Some(((received.saturating_mul(100) / total).min(99)) as u8)
            } else {
                None
            };
            emit(
                app,
                "download",
                format!(
                    "Downloading the official engine… {} MB",
                    received / 1_000_000
                ),
                percent,
            );
        }
    }
    file.flush()
        .map_err(|e| format!("Could not finish the download: {e}"))?;
    if total > 0 && received != total {
        return Err("The download ended early. Check your connection and try again.".into());
    }
    Ok(())
}

fn sha256(path: &Path) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn gpg_error(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let message = stderr
        .lines()
        .filter(|line| {
            !line.contains("keyboxd")
                && !line.contains("public key")
                && !line.contains("Total number processed")
                && !line.contains("imported:")
        })
        .collect::<Vec<_>>()
        .join(" ");
    if message.trim().is_empty() {
        format!("GnuPG exited with status {}.", output.status)
    } else {
        message.trim().to_string()
    }
}

fn verify_signature(work: &Path, archive: &Path, signature: &Path) -> Result<(), String> {
    let key_path = work.join("eddie_maintainer_gpg.key");
    let home = work.join("gnupg");
    fs::write(&key_path, EDDIE_KEY)
        .map_err(|e| format!("Could not prepare AirVPN's public key: {e}"))?;
    private_dir(&home)?;

    let imported = Command::new("gpg")
        .arg("--homedir")
        .arg(&home)
        .args(["--batch", "--no-tty", "--import"])
        .arg(&key_path)
        .output()
        .map_err(|e| {
            format!(
                "Automatic setup needs GnuPG (gpg and gpgv) to verify AirVPN's signature, but it could not be started: {e}"
            )
        })?;
    if !imported.status.success() {
        return Err(format!(
            "Could not load AirVPN's signing key: {}",
            gpg_error(&imported)
        ));
    }

    // Do not trust whatever key happened to be in the file: check the pinned
    // primary-key fingerprint before asking gpgv to validate the archive.
    let fingerprints = Command::new("gpg")
        .arg("--homedir")
        .arg(&home)
        .args(["--batch", "--with-colons", "--fingerprint"])
        .output()
        .map_err(|e| format!("Could not inspect AirVPN's signing key: {e}"))?;
    if !String::from_utf8_lossy(&fingerprints.stdout).contains(EDDIE_KEY_FINGERPRINT) {
        return Err("AirVPN's signing key did not match the pinned fingerprint.".into());
    }

    let verified = Command::new("gpgv")
        .arg("--homedir")
        .arg(&home)
        .arg("--keyring")
        .arg(home.join("pubring.kbx"))
        .arg(signature)
        .arg(archive)
        .output()
        .map_err(|e| {
            format!(
                "Automatic setup needs GnuPG (gpg and gpgv) to verify AirVPN's signature, but gpgv could not be started: {e}"
            )
        })?;
    if !verified.status.success() {
        return Err(format!(
            "The engine download did not pass AirVPN's signature check: {}",
            gpg_error(&verified)
        ));
    }
    Ok(())
}

fn find_appimage(directory: &Path) -> Result<PathBuf, String> {
    let entries = fs::read_dir(directory)
        .map_err(|e| format!("Could not inspect the unpacked engine: {e}"))?;
    let mut fallback = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == "Eddie-VPN" || name == "Eddie" || name == "eddie-ui" {
            return Ok(path);
        }
        if fallback.is_none() && !name.ends_with(".asc") {
            fallback = Some(path);
        }
    }
    fallback.ok_or_else(|| "The downloaded archive did not contain the Eddie AppImage.".into())
}

/// Install the official Eddie engine and its privileged helper, then return
/// the newly discovered engine description. Runs on a blocking worker.
pub fn install(app: AppHandle) -> Result<EngineInfo, String> {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("The engine is already being installed.".into());
    }
    let _guard = InstallGuard;
    let result = install_inner(&app);
    if let Err(error) = &result {
        emit(&app, "error", error.clone(), None);
    }
    result
}

fn install_inner(app: &AppHandle) -> Result<EngineInfo, String> {
    let existing = engine::locate();
    if existing.found {
        return Ok(existing);
    }

    if !cfg!(target_os = "linux") {
        return Err(
            "Automatic engine setup is currently available on Linux. Install the official Eddie client for your platform, then check again."
                .into(),
        );
    }
    if std::env::consts::ARCH != "x86_64" {
        return Err(
            "Automatic setup currently supports x86_64 Linux. Install the official Eddie package for your architecture, then check again."
                .into(),
        );
    }

    emit(app, "download", "Contacting AirVPN…", Some(0));
    let client = client()?;

    let download_url = "https://eddie.website/download/?platform=linux&arch=x64&ui=ui&format=appimage.AppImage&version=stable";
    let archive_info: DownloadInfo = get_json(&client, download_url)?;
    ensure_official(&archive_info.url)?;
    if !archive_info.filename.ends_with(".tar.gz") {
        return Err("AirVPN returned an unexpected engine archive format.".into());
    }

    let checksum_url = "https://eddie.website/api/download/?platform=linux&arch=x64&ui=ui&format=appimage.AppImage&version=stable&checksum=sha256";
    let checksum: ChecksumInfo = get_json(&client, checksum_url)?;
    if checksum.checksum.len() != 64 {
        return Err("AirVPN returned an invalid SHA-256 checksum.".into());
    }

    let signature_url = "https://eddie.website/api/download/?platform=linux&arch=x64&ui=ui&format=appimage.AppImage&version=stable&pgp_signature=1";
    let signature_info: SignatureInfo = get_json(&client, signature_url)?;
    ensure_official(&signature_info.signature_url)?;

    let work = cache_root().join(format!("nimbus/install-{}", std::process::id()));
    private_dir(&work)?;
    let work = fs::canonicalize(&work)
        .map_err(|e| format!("Could not resolve the install directory: {e}"))?;
    let _temporary = TempDir(work.clone());
    let archive = work.join("eddie.tar.gz");
    let signature = work.join("eddie.tar.gz.asc");

    emit(
        app,
        "download",
        format!("Downloading {} from eddie.website…", archive_info.filename),
        Some(0),
    );
    download(app, &client, &archive_info.url, &archive, true)?;

    emit(app, "verify", "Checking SHA-256 checksum…", None);
    let digest = sha256(&archive)?;
    if !digest.eq_ignore_ascii_case(&checksum.checksum) {
        return Err("The engine download failed its SHA-256 checksum check.".into());
    }

    emit(app, "verify", "Verifying AirVPN's OpenPGP signature…", None);
    download(
        app,
        &client,
        &signature_info.signature_url,
        &signature,
        false,
    )?;
    verify_signature(&work, &archive, &signature)?;

    emit(app, "extract", "Unpacking the official engine…", None);
    let extract = work.join("extract");
    fs::create_dir_all(&extract)
        .map_err(|e| format!("Could not prepare the unpack directory: {e}"))?;
    let extracted = Command::new("tar")
        .args(["-xzf"])
        .arg(&archive)
        .arg("-C")
        .arg(&extract)
        .output()
        .map_err(|e| format!("Could not unpack the engine archive: {e}"))?;
    if !extracted.status.success() {
        return Err("The engine archive could not be unpacked.".into());
    }

    let appimage = find_appimage(&extract)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&appimage, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Could not make the AppImage executable: {e}"))?;
    }

    let unpacked = Command::new(&appimage)
        .arg("--appimage-extract")
        .current_dir(&extract)
        .output()
        .map_err(|e| format!("Could not extract the Eddie AppImage: {e}"))?;
    if !unpacked.status.success() {
        return Err(
            "The Eddie AppImage could not be extracted. Your system may not be able to run AppImages."
                .into(),
        );
    }

    let source = extract.join("squashfs-root/opt/eddie-ui");
    if !source.join("eddie-cli").is_file() {
        return Err("The downloaded archive did not contain eddie-cli.".into());
    }
    if !source.join("eddie-cli-elevated").is_file() {
        return Err("The downloaded archive did not contain Eddie's privileged helper.".into());
    }

    emit(
        app,
        "install",
        "Installing to /opt/eddie-ui. Approve the administrator prompt…",
        None,
    );

    if !Path::new(INSTALL_DIR).join("eddie-cli").is_file() {
        // A fixed script is passed to the administrator shell. The only variable
        // is the verified source directory; no user-controlled shell fragment is
        // interpreted by the privileged process.
        const INSTALL_SCRIPT: &str = r#"
set -eu
src=$1
dest=/opt/eddie-ui
if [ ! -d "$src" ]; then echo "Source directory is missing" >&2; exit 10; fi
if [ ! -x "$src/eddie-cli" ]; then echo "eddie-cli is missing" >&2; exit 11; fi
if [ ! -x "$src/eddie-cli-elevated" ]; then echo "eddie-cli-elevated is missing" >&2; exit 12; fi
if [ -e "$dest/eddie-cli" ]; then echo "Eddie is already installed in $dest" >&2; exit 13; fi
install -d -m 0755 "$dest"
cp -a "$src"/. "$dest"/
chown -R root:root "$dest"
for f in eddie-cli eddie-cli-elevated eddie-cli-elevated-service; do
  if [ -e "$dest/$f" ]; then chmod 0755 "$dest/$f"; fi
done
"$dest/eddie-cli-elevated" service=install service_port=9350
"#;

        let output = Command::new("pkexec")
            .args(["/bin/sh", "-c", INSTALL_SCRIPT, "nimbus-install"])
            .arg(&source)
            .output()
            .map_err(|e| {
                format!(
                    "Automatic setup needs polkit's pkexec to install the privileged helper, but pkexec could not be started: {e}"
                )
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let detail = format!("{} {}", stdout.trim(), stderr.trim())
                .trim()
                .to_string();
            let message = if output.status.code() == Some(126) || output.status.code() == Some(127)
            {
                "Administrator approval was cancelled. The engine was not installed.".to_string()
            } else if detail.is_empty() {
                format!(
                    "The privileged installer exited with status {}. The engine was not installed.",
                    output.status
                )
            } else {
                format!("The privileged installer failed: {detail}")
            };
            return Err(message);
        }
    }

    if !Path::new(INSTALL_DIR).join("eddie-cli").is_file() {
        return Err("The installer finished, but eddie-cli was not found in /opt/eddie-ui.".into());
    }

    emit(app, "done", "The engine is ready.", Some(100));
    let installed = engine::locate();
    if !installed.found {
        return Err(
            "Eddie was installed but could not be located. Restart Nimbus, then try again.".into(),
        );
    }
    Ok(installed)
}

#[tauri::command]
pub async fn engine_install(app: AppHandle) -> Result<EngineInfo, String> {
    tauri::async_runtime::spawn_blocking(move || install(app))
        .await
        .map_err(|e| format!("Engine installer task failed: {e}"))?
}
