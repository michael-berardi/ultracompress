//! CLI side of the 2026-11-26 update contract.
//!
//! This is the only module in the tool that opens a connection. It makes one
//! anonymous GET to the public GitHub latest-release record, at most daily,
//! with a four-second ceiling. Everything else (gates, cache, notices) lives
//! in `ultracompress_core::update` and is testable offline.

use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use ultracompress_core::update as contract;

#[derive(Debug)]
pub struct Outcome {
    pub release: Option<contract::Release>,
    pub error: Option<String>,
    pub gate: Option<contract::Gate>,
}

/// One anonymous request. No query parameters, credentials, install ids,
/// machine data or usage data; the user agent is fixed by the contract.
pub fn fetch_latest(url: &str) -> Result<contract::Release, String> {
    let agent = ureq::Agent::config_builder()
        .user_agent(contract::USER_AGENT)
        .timeout_global(Some(Duration::from_secs(contract::CHECK_TIMEOUT_SECS)))
        .timeout_connect(Some(Duration::from_secs(2)))
        .build()
        .new_agent();
    let response = agent
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("GET failed: {e}"))?;
    let status = response.status().as_u16();
    let mut body = response.into_body();
    let body = body
        .read_to_string()
        .map_err(|e| format!("could not read release record: {e}"))?;
    if status != 200 {
        return Err(format!("HTTP {status} from release API"));
    }
    contract::release_from_json(&body)
}

fn latest_url() -> Result<String, String> {
    contract::latest_release_url(env!("CARGO_PKG_REPOSITORY"))
}

fn download_base() -> Result<String, String> {
    let url = latest_url()?;
    url.strip_suffix("/releases/latest")
        .map(str::to_string)
        .ok_or_else(|| "cannot derive release download base".to_string())
}

/// The automatic check: never in CI, non-TTY or agent mode, no retry loop.
/// Failures are written to the cache with their cause and never printed.
pub fn check(_current: &str, interactive: bool, explicit: bool) -> Outcome {
    let config = contract::read_config();
    let gate = contract::gate(interactive, explicit, &config);
    if gate != contract::Gate::Allowed {
        return Outcome {
            release: None,
            error: None,
            gate: Some(gate),
        };
    }
    let mut cache = contract::read_cache();
    let now = contract::now_secs();
    if contract::check_due(&cache, now) {
        match latest_url().and_then(|url| fetch_latest(&url)) {
            Ok(release) => {
                cache.checked_at = now;
                cache.last_error = None;
                cache.release = Some(release);
            }
            Err(cause) => {
                cache.checked_at = now;
                cache.last_error = Some(cause);
            }
        }
        let _ = contract::write_cache(&cache);
    }
    Outcome {
        release: cache.release,
        error: cache.last_error,
        gate: None,
    }
}

fn spawn_detached(arg: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find own binary: {e}"))?;
    Command::new(exe)
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("cannot start background update worker: {e}"))
}

/// Called after the user's command has printed its output. A cached notice is
/// shown once per 24 h per version; a fresh check runs in a detached child so
/// the user's command never waits on the network.
pub fn after_command(current: &str, interactive: bool) {
    let mut cache = contract::read_cache();
    let config = contract::read_config();
    if contract::gate(interactive, false, &config) != contract::Gate::Allowed {
        return;
    }
    let now = contract::now_secs();
    let shown_version = cache.release.as_ref().and_then(|release| {
        if contract::showable(&cache, release, current, now) {
            let lines = contract::notice(release, current);
            if interactive {
                // Second line dim, as specified by the CLI notice contract.
                let mut parts = lines.lines();
                if let Some(first) = parts.next() {
                    eprintln!("{first}");
                }
                if let Some(second) = parts.next() {
                    eprintln!("\x1b[2m{second}\x1b[0m");
                }
            } else {
                eprintln!("{lines}");
            }
            Some(release.version.clone())
        } else {
            None
        }
    });
    if let Some(version) = shown_version {
        contract::mark_shown(&mut cache, &version, now);
        let _ = contract::write_cache(&cache);
    }
    if contract::check_due(&cache, now) {
        let _ = spawn_detached("--update-check-background");
    }
    if config.auto_install {
        if let Some(release) = &cache.release {
            if contract::is_newer(&release.version, current)
                && cache.auto_attempted_version.as_deref() != Some(release.version.as_str())
                && cache.skipped_version.as_deref() != Some(release.version.as_str())
            {
                cache.auto_attempted_version = Some(release.version.clone());
                let _ = contract::write_cache(&cache);
                let _ = spawn_detached("--update-auto-install");
            }
        }
    }
}

/// Hidden worker entry point: same gates and daily cache, no terminal needed.
pub fn background_check(current: &str) {
    let _ = check(current, false, true);
}

pub fn background_auto_install(current: &str) {
    let config = contract::read_config();
    if !config.auto_install {
        return;
    }
    let cache = contract::read_cache();
    let Some(release) = cache.release else { return };
    if !contract::is_newer(&release.version, current) {
        return;
    }
    if let Err(cause) = install_release(&release, false) {
        // Auto failure never breaks the current version: log the cause and
        // let the next interactive session fall back to the prompt.
        log_install(&format!(
            "auto install of {} failed: {cause}",
            release.version
        ));
        let mut cache = contract::read_cache();
        cache.last_error = Some(format!("auto install failed: {cause}"));
        cache.auto_attempted_version = None;
        let _ = contract::write_cache(&cache);
    }
}

/// Contract §4a: one bounded install log (256 KB max), never an unbounded
/// append. Old bytes are dropped from the front; the newest line is kept.
pub const MAX_INSTALL_LOG_BYTES: usize = 256 * 1024;
const MAX_INSTALL_LOG_LINE_CHARS: usize = 1000;

fn write_install_log(path: &Path, line: &str, now: u64) -> Result<(), String> {
    let brief: String = line.chars().take(MAX_INSTALL_LOG_LINE_CHARS).collect();
    let mut data = std::fs::read(path).unwrap_or_default();
    data.extend_from_slice(format!("{now} {brief}\n").as_bytes());
    if data.len() > MAX_INSTALL_LOG_BYTES {
        let cut = data.len() - MAX_INSTALL_LOG_BYTES;
        let mut start = data[cut..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|offset| cut + offset + 1)
            .unwrap_or(cut);
        // Skip UTF-8 continuation bytes so the retained tail stays valid.
        while start < data.len() && (data[start] & 0xC0) == 0x80 {
            start += 1;
        }
        data.drain(..start);
    }
    let parent = path
        .parent()
        .ok_or_else(|| "install log has no parent directory".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| format!("cannot create log dir: {e}"))?;
    let tmp = parent.join(format!(".install-{}-{}.log.tmp", std::process::id(), now));
    std::fs::write(&tmp, &data).map_err(|e| format!("cannot write install log: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("cannot protect install log: {e}"))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot publish install log: {e}")
    })
}

fn log_install(line: &str) {
    let path = contract::cache_dir().join("install.log");
    let _ = write_install_log(&path, line, contract::now_secs());
}

// ---- Install safety -------------------------------------------------------

#[cfg(target_os = "macos")]
pub fn asset_name() -> &'static str {
    "UltraCompress-macos-universal.zip"
}
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn asset_name() -> &'static str {
    "ultracompress-linux-x86_64.tar.gz"
}
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub fn asset_name() -> &'static str {
    "ultracompress-linux-arm64.tar.gz"
}
#[cfg(target_os = "windows")]
pub fn asset_name() -> &'static str {
    "ultracompress-windows-x86_64.zip"
}
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn asset_name() -> &'static str {
    ""
}

/// The checksum line for one asset in SHA256SUMS. Names may be prefixed with
/// `*`, as produced by `shasum` in binary mode.
pub fn checksum_for(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let hex = fields.next()?;
        let name = fields.next()?.trim_start_matches('*');
        if name == asset && hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            Some(hex.to_ascii_lowercase())
        } else {
            None
        }
    })
}

pub fn verify_checksum(sums: &str, asset: &str, bytes: &[u8]) -> Result<(), String> {
    let expected = checksum_for(sums, asset)
        .ok_or_else(|| format!("no checksum for {asset} in SHA256SUMS; nothing was installed"))?;
    let got = format!("{:x}", Sha256::digest(bytes));
    if got != expected {
        return Err(format!(
            "checksum mismatch for {asset}; nothing was installed"
        ));
    }
    Ok(())
}

/// Package manager ownership is detected before any file is replaced.
pub fn package_manager_command(target: &Path) -> Option<String> {
    let text = target.to_string_lossy().replace('\\', "/");
    if text.contains("/.cargo/") {
        Some(format!(
            "cargo install ultracompress --locked --version {}",
            env!("CARGO_PKG_VERSION")
        ))
    } else if text.contains("/Cellar/")
        || text.contains("/homebrew/")
        || text.contains("/Linuxbrew/")
    {
        Some("brew upgrade ultracompress".to_string())
    } else if text.contains("/node_modules/.bin/") {
        Some("npm upgrade -g ultracompress".to_string())
    } else {
        None
    }
}

fn install_target() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find own binary: {e}"))?;
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    // A shim points at the real installer-managed binary; never replace the shim.
    if let Some(parent) = exe.parent() {
        if parent
            .to_string_lossy()
            .replace('\\', "/")
            .contains("/.local/bin")
            && !home.is_empty()
        {
            let real = Path::new(&home).join(".ultracompress/ultracompress");
            if real.is_file() {
                return Ok(real);
            }
        }
    }
    Ok(exe)
}

/// Atomic swap with exactly one rollback copy. On any failure the old binary
/// stays (or is restored) and the new one applies on the next start.
pub fn swap_binary(staged: &Path, target: &Path) -> Result<(), String> {
    if !staged.is_file() {
        return Err("downloaded binary is missing".to_string());
    }
    let parent = target
        .parent()
        .ok_or_else(|| "install target has no parent".to_string())?;
    let previous = target.with_extension("previous");
    if target.exists() {
        std::fs::copy(target, &previous).map_err(|e| format!("cannot keep rollback copy: {e}"))?;
    }
    let tmp = parent.join(format!(
        ".ultracompress-new-{}-{}",
        std::process::id(),
        contract::now_secs()
    ));
    std::fs::copy(staged, &tmp).map_err(|e| format!("cannot stage new binary: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot make new binary executable: {e}"))?;
    }
    if let Err(err) = std::fs::rename(&tmp, target) {
        let _ = std::fs::remove_file(&tmp);
        if previous.exists() && target.exists() {
            let _ = std::fs::copy(&previous, target);
        }
        return Err(format!("atomic swap failed: {err}"));
    }
    Ok(())
}

pub fn rollback(target: &Path) -> Result<(), String> {
    let previous = target.with_extension("previous");
    if !previous.exists() {
        return Err("no rollback copy found".to_string());
    }
    let tmp = target.with_extension("rollback-tmp");
    std::fs::copy(&previous, &tmp).map_err(|e| format!("cannot stage rollback: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
    }
    std::fs::rename(&tmp, target).map_err(|e| format!("rollback failed: {e}"))
}

fn download(url: &str, dest: &Path, timeout: u64) -> Result<(), String> {
    let agent = ureq::Agent::config_builder()
        .user_agent(contract::USER_AGENT)
        .timeout_global(Some(Duration::from_secs(timeout)))
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    if response.status().as_u16() != 200 {
        return Err(format!("download HTTP {}", response.status().as_u16()));
    }
    let mut body = response.body_mut().as_reader();
    let mut file =
        std::fs::File::create(dest).map_err(|e| format!("cannot stage download: {e}"))?;
    std::io::copy(&mut body, &mut file).map_err(|e| format!("download read failed: {e}"))?;
    Ok(())
}

fn unpack(archive: &Path, stage: &Path) -> Result<(), String> {
    let name = archive.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let status = if cfg!(windows) {
        Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Expand-Archive",
                "-Path",
                &archive.to_string_lossy(),
                "-DestinationPath",
                &stage.to_string_lossy(),
                "-Force",
            ])
            .status()
    } else {
        Command::new("tar")
            .args([
                "-xf",
                &archive.to_string_lossy(),
                "-C",
                &stage.to_string_lossy(),
            ])
            .status()
    };
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("cannot unpack {name} (exit {s})")),
        Err(e) => Err(format!("cannot unpack {name}: {e}")),
    }
}

fn find_binary(dir: &Path) -> Option<PathBuf> {
    let wanted = if cfg!(windows) {
        "ultracompress.exe"
    } else {
        "ultracompress"
    };
    let mut found = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some(wanted) {
                found = Some(path);
            }
        }
    }
    found
}

/// Verify the release checksum before swapping anything, then replace the
/// binary atomically. A package-managed install is never overwritten.
pub fn install_release(release: &contract::Release, quiet: bool) -> Result<(), String> {
    let asset = asset_name();
    if asset.is_empty() {
        return Err("no release asset for this platform; upgrade from source".to_string());
    }
    let target = install_target()?;
    if let Some(command) = package_manager_command(&target) {
        return Err(format!("package-managed install; run: {command}"));
    }
    let base = download_base()?;
    let tag = format!("v{}", release.version);
    let dir = contract::cache_dir().join("install").join(&tag);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create install stage: {e}"))?;
    let sums_path = dir.join("SHA256SUMS");
    let asset_path = dir.join(asset);
    download(&format!("{base}/download/{tag}/SHA256SUMS"), &sums_path, 30)?;
    download(&format!("{base}/download/{tag}/{asset}"), &asset_path, 30)?;
    let sums =
        std::fs::read_to_string(&sums_path).map_err(|e| format!("cannot read checksums: {e}"))?;
    let bytes = std::fs::read(&asset_path).map_err(|e| format!("cannot read download: {e}"))?;
    verify_checksum(&sums, asset, &bytes)?;
    let unpacked = dir.join("unpacked");
    let _ = std::fs::remove_dir_all(&unpacked);
    std::fs::create_dir_all(&unpacked).map_err(|e| format!("cannot stage unpack: {e}"))?;
    unpack(&asset_path, &unpacked)?;
    let staged =
        find_binary(&unpacked).ok_or_else(|| "release has no ultracompress binary".to_string())?;
    let reported = Command::new(&staged)
        .arg("--version")
        .output()
        .map_err(|e| format!("downloaded binary does not run: {e}"))?;
    let reported = String::from_utf8_lossy(&reported.stdout).trim().to_string();
    if reported != format!("ultracompress {}", release.version) {
        return Err(format!(
            "downloaded binary reports '{reported}', expected {}",
            release.version
        ));
    }
    swap_binary(&staged, &target)?;
    if !quiet {
        println!(
            "Installed ultracompress {}. The new version applies on the next start.",
            release.version
        );
        println!("Rollback: ultracompress update --rollback");
    }
    Ok(())
}

fn confirm(prompt: &str, default_yes: bool) -> bool {
    let marker = if default_yes { " [Y/n] " } else { " [y/N] " };
    print!("{prompt}{marker}");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return default_yes;
    }
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        "" => default_yes,
        _ => default_yes,
    }
}

/// `ultracompress update [--check|--skip|--auto on|off|status|--rollback]`.
/// Returns the process exit code: 0 ok, 10 update available, 1 check failed.
pub fn command(args: &[String], current: &str, interactive: bool) -> i32 {
    let mut config = contract::read_config();
    let mut cache = contract::read_cache();
    let flag = args.first().map(String::as_str).unwrap_or("");
    match flag {
        "--auto" => {
            let mode = args.get(1).map(String::as_str).unwrap_or("status");
            match mode {
                "on" => config.auto_install = true,
                "off" => config.auto_install = false,
                "status" => {}
                other => {
                    eprintln!("ultracompress: bad --auto value '{other}' (on|off|status)");
                    return 2;
                }
            }
            if mode != "status" {
                if let Err(cause) = contract::write_config(&config) {
                    eprintln!("ultracompress: cannot save update settings: {cause}");
                    return 1;
                }
            }
            println!(
                "automatic updates: {} · checks: {}",
                if config.auto_install { "on" } else { "off" },
                if config.check_enabled { "on" } else { "off" }
            );
            return 0;
        }
        "--rollback" => match install_target().and_then(|target| rollback(&target)) {
            Ok(()) => {
                println!("Restored the previous ultracompress binary.");
                return 0;
            }
            Err(cause) => {
                eprintln!("ultracompress: rollback failed: {cause}");
                return 1;
            }
        },
        "--check" => {
            let outcome = check(current, interactive, true);
            if let Some(gate) = outcome.gate {
                println!("ultracompress update checks disabled: {gate}");
                return 0;
            }
            let (text, code) =
                contract::check_report(outcome.release.as_ref(), outcome.error.as_deref(), current);
            println!("{text}");
            return code;
        }
        "--skip" => {
            let outcome = check(current, interactive, true);
            if let Some(gate) = outcome.gate {
                println!("ultracompress update checks disabled: {gate}");
                return 0;
            }
            let Some(release) = outcome.release else {
                let (text, code) = contract::check_report(None, outcome.error.as_deref(), current);
                println!("{text}");
                return code;
            };
            contract::skip(&mut cache, Some(&release.version));
            if let Err(cause) = contract::write_cache(&cache) {
                eprintln!("ultracompress: cannot save skipped version: {cause}");
                return 1;
            }
            println!(
                "Skipping notices for {} until a newer release.",
                release.version
            );
            return 0;
        }
        _ => {}
    }
    if !flag.is_empty() {
        eprintln!("ultracompress: unknown update option '{flag}'");
        return 2;
    }
    let outcome = check(current, interactive, true);
    if let Some(gate) = outcome.gate {
        println!("ultracompress update checks disabled: {gate}");
        return 0;
    }
    let Some(release) = outcome.release else {
        let (text, code) = contract::check_report(None, outcome.error.as_deref(), current);
        println!("{text}");
        return code;
    };
    if !contract::is_newer(&release.version, current) {
        println!("ultracompress is up to date ({current})");
        return 0;
    }
    println!("ultracompress {current} → {}", release.version);
    if !release.notes.is_empty() {
        println!("{}", release.notes);
    }
    if interactive && !confirm(&format!("Install {} now?", release.version), true) {
        return 0;
    }
    if let Err(cause) = install_release(&release, false) {
        eprintln!("ultracompress: install failed: {cause}");
        return 1;
    }
    contract::mark_shown(&mut cache, &release.version, contract::now_secs());
    let _ = contract::write_cache(&cache);
    if !config.auto_install && !config.auto_asked && interactive {
        let auto = confirm("Install future updates automatically?", false);
        config.auto_install = auto;
        config.auto_asked = true;
        if let Err(cause) = contract::write_config(&config) {
            eprintln!("ultracompress: cannot save update settings: {cause}");
            return 1;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_refuse_a_mismatch() {
        let sums =
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  tool.tar.gz\n";
        assert!(checksum_for(sums, "tool.tar.gz").is_some());
        assert!(checksum_for(sums, "other.tar.gz").is_none());
        assert!(verify_checksum(sums, "tool.tar.gz", b"payload").is_err());
        let good = format!("{:x}  tool.tar.gz", Sha256::digest(b"payload"));
        assert!(verify_checksum(&good, "tool.tar.gz", b"payload").is_ok());
    }

    #[test]
    fn rollback_has_exactly_one_previous_copy() {
        let dir = std::env::temp_dir().join(format!("uc-swap-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let target = dir.join("ultracompress");
        let staged = dir.join("new-binary");
        std::fs::write(&target, b"old").unwrap();
        std::fs::write(&staged, b"new").unwrap();
        swap_binary(&staged, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(
            std::fs::read(dir.join("ultracompress.previous")).unwrap(),
            b"old"
        );
        rollback(&target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_log_stays_bounded_at_256_kb() {
        let dir = std::env::temp_dir().join(format!("uc-log-test-{}", std::process::id()));
        let path = dir.join("install.log");
        let _ = std::fs::remove_dir_all(&dir);
        let filler = "x".repeat(16 * 1024);
        write_install_log(&path, &format!("old {filler}"), 1).unwrap();
        for stamp in 2..400 {
            write_install_log(&path, &format!("entry {stamp} {filler}"), stamp).unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() <= MAX_INSTALL_LOG_BYTES);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("entry 399 "));
        assert!(!text.contains("old "));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(!std::fs::read_dir(&dir).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn package_manager_is_never_overwritten() {
        assert!(package_manager_command(Path::new("/opt/homebrew/bin/ultracompress")).is_some());
        assert!(
            package_manager_command(Path::new("/home/someone/.cargo/bin/ultracompress")).is_some()
        );
        assert!(package_manager_command(Path::new("/usr/local/bin/ultracompress")).is_none());
    }
}
