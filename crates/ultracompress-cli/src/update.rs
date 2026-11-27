//! `ultracompress update` and the after-command notice (docs/updates.md).
//!
//! The check is one anonymous GET to the public GitHub latest-release record of
//! the repository in this crate's metadata, at most once a day, from a detached
//! background process with a 4 s timeout. Its result is one small cache file;
//! the user's choices (skip, automatic installs) live in `config.json`. HTTP
//! goes through `curl`, which the installers already require, so no HTTP crate
//! is added.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const USER_AGENT: &str = "ultracompress-update-check";
const DAY: u64 = 24 * 60 * 60;
const CHECK_TIMEOUT_SECS: u64 = 4;
const LOG_MAX_BYTES: u64 = 256 * 1024;
const NOTES_MAX_LINES: usize = 6;
const NOTES_MAX_CHARS: usize = 600;
const USAGE: &str =
    "usage: ultracompress update [--yes] | --check | --skip | --auto on|off|status | --rollback | --clean";

// Local retention (contract §4a): the rollback copy is tied to the version that
// installed it, not to the old binary's mtime, and install leftovers are swept.
const ROLLBACK_MAX_AGE_SECS: u64 = 7 * DAY;
const DEBRIS_MAX_AGE_SECS: u64 = 3_600;
const ROLLBACK_OBSERVED_VERSION: &str = "observed";

type Version = (u64, u64, u64);

/// A stable `x.y.z`, optionally `v`-prefixed. Prereleases and anything else are not versions.
fn parse_version(text: &str) -> Option<Version> {
    let text = text.trim();
    let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
    let mut numbers = [0u64; 3];
    for slot in &mut numbers {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    parts
        .next()
        .is_none()
        .then_some((numbers[0], numbers[1], numbers[2]))
}

fn text((major, minor, patch): Version) -> String {
    format!("{major}.{minor}.{patch}")
}

fn current() -> Version {
    parse_version(VERSION).unwrap_or_default()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn env_var(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

// ---------------------------------------------------------------- state

struct Paths {
    cache: PathBuf,
    config: PathBuf,
    log: PathBuf,
}

impl Paths {
    fn new() -> Self {
        Self::from_env(|key| env_var(key).map(PathBuf::from))
    }

    fn from_env(var: impl Fn(&str) -> Option<PathBuf>) -> Self {
        let home = var("HOME")
            .or_else(|| var("USERPROFILE"))
            .unwrap_or_else(|| PathBuf::from("."));
        let (cache_root, config_root) = if cfg!(windows) {
            (
                var("LOCALAPPDATA").unwrap_or_else(|| home.join("AppData").join("Local")),
                var("APPDATA").unwrap_or_else(|| home.join("AppData").join("Roaming")),
            )
        } else if cfg!(target_os = "macos") {
            (
                home.join("Library").join("Caches"),
                home.join("Library").join("Application Support"),
            )
        } else {
            (
                var("XDG_CACHE_HOME").unwrap_or_else(|| home.join(".cache")),
                var("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config")),
            )
        };
        let cache_dir =
            var("ULTRACOMPRESS_CACHE_DIR").unwrap_or_else(|| cache_root.join("ultracompress"));
        let config_dir =
            var("ULTRACOMPRESS_CONFIG_DIR").unwrap_or_else(|| config_root.join("ultracompress"));
        Self {
            cache: cache_dir.join("update-check.json"),
            log: cache_dir.join("update.log"),
            config: config_dir.join("config.json"),
        }
    }
}

/// The one cache file the check writes.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Cache {
    /// Last check (Unix seconds); the next waits a day, also after a failure.
    checked_at: u64,
    /// Newest stable release seen, e.g. "0.8.0", and the first lines of its notes.
    #[serde(skip_serializing_if = "Option::is_none")]
    latest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
    /// Cause of the last failed check, shown by `update --check`.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// Version last announced and when: one notice per version per day.
    #[serde(skip_serializing_if = "Option::is_none")]
    noticed: Option<String>,
    noticed_at: u64,
    /// Version an automatic install ran for, and why it failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    auto_tried: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    auto_error: Option<String>,
    /// Version the last self-update installed; its first successful run drops `.previous`.
    #[serde(skip_serializing_if = "Option::is_none")]
    installed: Option<String>,
}

/// The user's choices. Automatic installs are off until the user opts in.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Config {
    check_automatically: bool,
    install_automatically: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    skip_version: Option<String>,
    /// "Install future updates automatically?" was answered; never asked again.
    asked_auto: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            check_automatically: true,
            install_automatically: false,
            skip_version: None,
            asked_auto: false,
        }
    }
}

/// Missing or unreadable state starts from the defaults.
fn read<T: Default + DeserializeOwned>(path: &Path) -> T {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Atomic, owner-only write of one small JSON file.
fn write<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let temp = sibling(path, &format!(".{}.tmp", std::process::id()));
    let result = (|| -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
        private_file(
            fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true),
        )
        .open(&temp)?
        .write_all(&bytes)?;
        fs::rename(&temp, path)
    })();
    result.map_err(|e| {
        let _ = fs::remove_file(&temp);
        format!("cannot write {}: {e}", path.display())
    })
}

fn private_file(options: &mut fs::OpenOptions) -> &mut fs::OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Append one line to the background-install log, starting over past 256 KB.
fn log(path: &Path, line: &str) {
    let line = format!("{} {line}\n", now());
    let full = fs::metadata(path).map_or(0, |m| m.len()) + line.len() as u64 > LOG_MAX_BYTES;
    let mut options = fs::OpenOptions::new();
    if full {
        options.write(true).truncate(true);
    } else {
        options.append(true);
    }
    if let Ok(mut file) = private_file(options.create(true)).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}

// ---------------------------------------------------------------- check

/// Opt-outs and hosts that must stay silent: the env switch, CI, Claude Code.
fn quiet(var: impl Fn(&str) -> Option<String>) -> bool {
    let on = |key: &str| var(key).is_some_and(|v| v != "0" && !v.eq_ignore_ascii_case("false"));
    on("ULTRACOMPRESS_NO_UPDATE_CHECK") || on("CI") || on("CLAUDECODE")
}

struct Release {
    version: Version,
    notes: String,
    /// `(name, browser_download_url)` from the release record.
    assets: Vec<(String, String)>,
}

impl Release {
    fn url(&self, name: &str) -> Result<&str, String> {
        self.assets
            .iter()
            .find(|(asset, _)| asset == name)
            .map(|(_, url)| url.as_str())
            .ok_or_else(|| format!("release {} has no {name}", text(self.version)))
    }
}

fn first_lines(notes: &str) -> String {
    let lines: Vec<&str> = notes
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .take(NOTES_MAX_LINES)
        .collect();
    let joined = lines.join("\n");
    if joined.chars().count() <= NOTES_MAX_CHARS {
        return joined;
    }
    joined.chars().take(NOTES_MAX_CHARS).collect::<String>() + "…"
}

/// `Ok(None)`: a draft, a prerelease, or no stable `x.y.z` tag. Never newer.
fn parse_release(value: &Value) -> Result<Option<Release>, String> {
    if !value.is_object() {
        return Err("the release record is not a JSON object".into());
    }
    let flag = |key: &str| value.get(key).and_then(Value::as_bool).unwrap_or(false);
    if flag("draft") || flag("prerelease") {
        return Ok(None);
    }
    let Some(version) = value
        .get("tag_name")
        .and_then(Value::as_str)
        .and_then(parse_version)
    else {
        return Ok(None);
    };
    let assets = value
        .get("assets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|asset| {
            let name = asset.get("name")?.as_str()?;
            let url = asset.get("browser_download_url")?.as_str()?;
            Some((name.to_string(), url.to_string()))
        })
        .collect();
    let notes = first_lines(value.get("body").and_then(Value::as_str).unwrap_or(""));
    Ok(Some(Release {
        version,
        notes,
        assets,
    }))
}

fn api_url() -> String {
    env_var("ULTRACOMPRESS_UPDATE_API").unwrap_or_else(|| {
        let slug = REPOSITORY
            .trim_start_matches("https://github.com/")
            .trim_end_matches('/')
            .trim_end_matches(".git");
        format!("https://api.github.com/repos/{slug}/releases/latest")
    })
}

/// One anonymous GET: `-q` ignores `~/.curlrc`, so only the fixed User-Agent is
/// sent. Plain HTTP is allowed only when the release API is overridden (tests).
fn curl(url: &str, timeout: u64, max_bytes: u64, out: Option<&Path>) -> Result<Vec<u8>, String> {
    let proto = if env_var("ULTRACOMPRESS_UPDATE_API").is_some() {
        "=http,https"
    } else {
        "=https"
    };
    let mut command = Command::new("curl");
    command
        .args(["-q", "-fsSL", "--proto", proto, "--proto-redir", proto])
        .args(["-A", USER_AGENT, "--max-time", &timeout.to_string()])
        .args(["--max-filesize", &max_bytes.to_string()]);
    if let Some(out) = out {
        command.arg("-o").arg(out);
    }
    let output = command
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        return Err(if detail.is_empty() {
            format!("{url}: curl {}", output.status)
        } else {
            format!("{url}: {}", detail.chars().take(200).collect::<String>())
        });
    }
    if output.stdout.len() as u64 > max_bytes {
        return Err(format!("{url}: response over {max_bytes} bytes"));
    }
    Ok(output.stdout)
}

fn fetch_release() -> Result<Option<Release>, String> {
    let body = curl(&api_url(), CHECK_TIMEOUT_SECS, 1 << 20, None)?;
    let value: Value =
        serde_json::from_slice(&body).map_err(|e| format!("unreadable release record: {e}"))?;
    parse_release(&value)
}

fn record(cache: &mut Cache, result: &Result<Option<Release>, String>, at: u64) {
    cache.checked_at = at;
    match result {
        Ok(release) => {
            cache.latest = release.as_ref().map(|r| text(r.version));
            cache.notes = release
                .as_ref()
                .map(|r| r.notes.clone())
                .filter(|n| !n.is_empty());
            cache.error = None;
        }
        // The last known release stays, so a failed check never hides a notice.
        Err(cause) => cache.error = Some(cause.clone()),
    }
}

/// Explicit commands check now and record the outcome like the daily check.
fn check_now(paths: &Paths) -> Result<Option<Release>, String> {
    let result = fetch_release();
    let mut cache: Cache = read(&paths.cache);
    record(&mut cache, &result, now());
    let _ = write(&paths.cache, &cache);
    result.map(|release| release.filter(|r| r.version > current()))
}

fn fresh(cache: &Cache, at: u64) -> bool {
    cache.checked_at != 0 && cache.checked_at <= at && at - cache.checked_at < DAY
}

/// The newer release the user has not skipped.
fn pending(cache: &Cache, config: &Config, current: Version) -> Option<Version> {
    let latest = parse_version(cache.latest.as_deref()?)?;
    let skipped = config.skip_version.as_deref().and_then(parse_version);
    (latest > current && skipped.is_none_or(|s| latest > s)).then_some(latest)
}

/// An automatic install runs once per version; after a failure the notice returns.
fn auto_failed(cache: &Cache, version: &str) -> bool {
    cache.auto_tried.as_deref() == Some(version) && cache.auto_error.is_some()
}

#[derive(Debug, Default, PartialEq)]
struct Plan {
    /// Start the daily check in the background.
    refresh: bool,
    /// Start an opted-in install of this version in the background.
    install: Option<String>,
    /// Announce this version.
    notice: Option<String>,
}

fn plan(cache: &Cache, config: &Config, current: Version, at: u64) -> Plan {
    let refresh = !fresh(cache, at);
    let Some(version) = pending(cache, config, current).map(text) else {
        return Plan {
            refresh,
            ..Plan::default()
        };
    };
    if config.install_automatically && !auto_failed(cache, &version) {
        let start = cache.auto_tried.as_deref() != Some(&version);
        return Plan {
            refresh,
            install: start.then_some(version),
            notice: None,
        };
    }
    let due = cache.noticed.as_deref() != Some(&version)
        || cache.noticed_at > at
        || at - cache.noticed_at >= DAY;
    Plan {
        refresh,
        install: None,
        notice: due.then_some(version),
    }
}

fn notice_lines(new: &str, current: &str) -> [String; 2] {
    [
        format!("ultracompress {new} available (you have {current}) · run: ultracompress update"),
        "turn on automatic updates: ultracompress update --auto on · skip: ultracompress update --skip"
            .to_string(),
    ]
}

/// Shared by the terminal notice and `update --notice` (agent host footers):
/// starts the daily check or an opted-in install in the background and returns
/// the version to announce, if one is due.
fn notice_now() -> Option<String> {
    if quiet(env_var) {
        return None;
    }
    let paths = Paths::new();
    let config: Config = read(&paths.config);
    if !config.check_automatically {
        return None;
    }
    let mut cache: Cache = read(&paths.cache);
    let at = now();
    let plan = plan(&cache, &config, current(), at);
    if plan.refresh {
        // Claimed before the check starts: the next commands do not start another.
        cache.checked_at = at;
    }
    if plan.install.is_some() {
        cache.auto_tried.clone_from(&plan.install);
        cache.auto_error = None;
    }
    if let Some(version) = &plan.notice {
        cache.noticed = Some(version.clone());
        cache.noticed_at = at;
    }
    let background = plan.refresh || plan.install.is_some();
    if background || plan.notice.is_some() {
        let _ = write(&paths.cache, &cache);
    }
    if background {
        if let Err(cause) = start_background() {
            cache.error = Some(cause);
            let _ = write(&paths.cache, &cache);
        }
    }
    plan.notice
}

/// After a successful command: the notice only when both stdout and stderr are
/// a terminal (never in hook, pipe or JSON output). Local retention runs first
/// in `main`, on the same successful-command path.
pub fn after_command() {
    if !(io::stdout().is_terminal() && io::stderr().is_terminal()) {
        return;
    }
    if let Some(version) = notice_now() {
        let [first, second] = notice_lines(&version, VERSION);
        eprintln!("{first}");
        eprintln!("\x1b[2m{second}\x1b[0m");
    }
}

fn start_background() -> Result<(), String> {
    let exe = exe()?;
    let mut command = if cfg!(unix) {
        let mut nice = Command::new("nice");
        nice.args(["-n", "10"]).arg(&exe);
        nice
    } else {
        Command::new(&exe)
    };
    command
        .arg("__update-background")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        command.creation_flags(DETACHED_PROCESS | BELOW_NORMAL_PRIORITY_CLASS);
    }
    command
        .spawn()
        .map(drop)
        .map_err(|e| format!("cannot start the background check: {e}"))
}

/// The detached daily check; with automatic updates on, it also installs.
pub fn background() -> i32 {
    let paths = Paths::new();
    let result = fetch_release();
    let mut cache: Cache = read(&paths.cache);
    record(&mut cache, &result, now());
    let _ = write(&paths.cache, &cache);
    let Ok(Some(release)) = result else {
        return 0;
    };
    let config: Config = read(&paths.config);
    let version = text(release.version);
    if !config.install_automatically
        || pending(&cache, &config, current()) != Some(release.version)
        || auto_failed(&cache, &version)
    {
        return 0;
    }
    cache.auto_tried = Some(version.clone());
    cache.auto_error = None;
    let _ = write(&paths.cache, &cache);
    log(&paths.log, &format!("installing {version}"));
    let outcome = exe().and_then(|exe| match managed(&exe, &version) {
        Some((manager, command)) => Err(format!(
            "installed with {manager}; run: {}",
            command.join(" ")
        )),
        None => install(&release, &exe, &paths),
    });
    match outcome {
        Ok(()) => log(
            &paths.log,
            &format!("installed {version}; it runs from the next command"),
        ),
        Err(cause) => {
            log(&paths.log, &format!("install of {version} failed: {cause}"));
            let mut cache: Cache = read(&paths.cache);
            cache.auto_error = Some(cause);
            let _ = write(&paths.cache, &cache);
        }
    }
    0
}

// ---------------------------------------------------------------- install

fn exe() -> Result<PathBuf, String> {
    std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|e| format!("cannot locate the running binary: {e}"))
}

/// Package-manager installs are upgraded with the manager, never overwritten.
fn managed(exe: &Path, new: &str) -> Option<(&'static str, Vec<String>)> {
    let path = exe.to_string_lossy().replace('\\', "/");
    let cargo_home = env_var("CARGO_HOME").map(|home| home.replace('\\', "/") + "/bin/");
    if path.contains("/Cellar/")
        || path.starts_with("/opt/homebrew/")
        || path.starts_with("/home/linuxbrew/")
    {
        let command = ["brew", "upgrade", "ultracompress"];
        Some(("Homebrew", command.map(String::from).to_vec()))
    } else if path.contains("/.cargo/bin/") || cargo_home.is_some_and(|bin| path.starts_with(&bin))
    {
        let tag = format!("v{new}");
        let command = [
            "cargo",
            "install",
            "--locked",
            "--git",
            REPOSITORY,
            "--tag",
            &tag,
            "ultracompress",
        ];
        Some(("Cargo", command.map(String::from).to_vec()))
    } else {
        None
    }
}

fn asset_name() -> Result<&'static str, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", _) => Ok("UltraCompress-macos-universal.zip"),
        ("linux", "x86_64") => Ok("ultracompress-linux-x86_64.tar.gz"),
        ("linux", "aarch64") => Ok("ultracompress-linux-arm64.tar.gz"),
        ("windows", "x86_64") => Ok("ultracompress-windows-x86_64.zip"),
        (os, arch) => Err(format!("no release download for {os}/{arch}")),
    }
}

fn checksum_for(sums: &str, name: &str) -> Result<String, String> {
    sums.lines()
        .filter_map(|line| line.split_once(char::is_whitespace))
        .find(|(hash, file)| {
            file.trim().trim_start_matches('*') == name
                && hash.len() == 64
                && hash.bytes().all(|b| b.is_ascii_hexdigit())
        })
        .map(|(hash, _)| hash.to_ascii_lowercase())
        .ok_or_else(|| format!("SHA256SUMS has no entry for {name}; nothing was installed"))
}

/// Download folder, removed on success and on failure.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self, String> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let path = std::env::temp_dir().join(format!(
            "ultracompress-update-{}-{nanos}",
            std::process::id()
        ));
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        Ok(Self(path))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Download, verify against `SHA256SUMS`, prove the binary runs, then swap.
fn install(release: &Release, exe: &Path, paths: &Paths) -> Result<(), String> {
    let new = text(release.version);
    let asset = asset_name()?;
    let (asset_url, sums_url) = (release.url(asset)?, release.url("SHA256SUMS")?);
    let temp = TempDir::new()?;
    let archive = temp.0.join(asset);
    curl(asset_url, 300, 256 << 20, Some(&archive))?;
    let sums = curl(sums_url, 30, 1 << 20, None)?;
    let expected = checksum_for(&String::from_utf8_lossy(&sums), asset)?;
    let bytes = fs::read(&archive).map_err(|e| format!("cannot read the download: {e}"))?;
    if format!("{:x}", Sha256::digest(&bytes)) != expected {
        return Err(format!(
            "checksum mismatch for {asset}; nothing was installed"
        ));
    }
    let unpacked = temp.0.join("unpacked");
    fs::create_dir(&unpacked).map_err(|e| format!("cannot unpack: {e}"))?;
    let tar = Command::new("tar")
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(&unpacked)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run tar: {e}"))?;
    if !tar.status.success() {
        return Err(format!("cannot unpack {asset}"));
    }
    let name = if cfg!(windows) {
        "ultracompress.exe"
    } else {
        "ultracompress"
    };
    let staged = [unpacked.join("cli").join(name), unpacked.join(name)]
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| format!("{asset} holds no {name}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&staged, fs::Permissions::from_mode(0o755));
    }
    let probe = Command::new(&staged)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("the downloaded binary does not run: {e}"))?;
    let want = format!("ultracompress {new}");
    if !probe.status.success() || String::from_utf8_lossy(&probe.stdout).trim() != want {
        return Err(format!(
            "the downloaded binary does not report {want}; nothing was installed"
        ));
    }
    swap(exe, &staged)?;
    // The sidecar names the installed version and the swap time. The old
    // binary's mtime is never used as the install time.
    record_rollback(&sibling(exe, ".previous"), &new, now());
    let mut cache: Cache = read(&paths.cache);
    cache.installed = Some(new);
    let _ = write(&paths.cache, &cache);
    Ok(())
}

/// Replace `exe` with `staged`, keeping exactly one `<exe>.previous`.
fn swap(exe: &Path, staged: &Path) -> Result<(), String> {
    let next = sibling(exe, ".next");
    let previous = sibling(exe, ".previous");
    fs::copy(staged, &next).map_err(|e| {
        let _ = fs::remove_file(&next);
        format!("cannot stage the new binary: {e}; nothing was installed")
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&next, fs::Permissions::from_mode(0o755));
    }
    let result = place(exe, &next, &previous);
    if result.is_err() {
        let _ = fs::remove_file(&next);
    }
    result
}

/// Unix: copy the old binary aside, then one atomic rename.
#[cfg(not(windows))]
fn place(exe: &Path, next: &Path, previous: &Path) -> Result<(), String> {
    fs::copy(exe, previous).map_err(|e| {
        let _ = fs::remove_file(previous);
        format!("cannot keep the previous binary: {e}; nothing was installed")
    })?;
    // The copy keeps the old timestamp; the 7-day limit counts from now.
    if let Ok(file) = fs::OpenOptions::new().write(true).open(previous) {
        let _ = file.set_modified(SystemTime::now());
    }
    fs::rename(next, exe).map_err(|e| {
        let _ = fs::remove_file(previous);
        format!("cannot install the new binary: {e}; nothing was installed")
    })
}

/// Windows cannot overwrite a running binary but can rename it.
#[cfg(windows)]
fn place(exe: &Path, next: &Path, previous: &Path) -> Result<(), String> {
    let _ = fs::remove_file(previous);
    fs::rename(exe, previous)
        .map_err(|e| format!("cannot keep the previous binary: {e}; nothing was installed"))?;
    fs::rename(next, exe).map_err(|e| {
        let _ = fs::rename(previous, exe);
        format!("cannot install the new binary: {e}; nothing was installed")
    })
}

fn rollback(exe: &Path) -> Result<(), String> {
    let previous = sibling(exe, ".previous");
    if !previous.is_file() {
        return Err(format!(
            "no previous binary at {}; nothing to roll back",
            previous.display()
        ));
    }
    restore_previous(exe, &previous)?;
    let _ = fs::remove_file(rollback_sidecar(&previous));
    Ok(())
}

/// Windows cannot overwrite a running binary: swap names. The replaced file
/// becomes the new `.previous` and goes with the 7-day limit.
#[cfg(windows)]
fn restore_previous(exe: &Path, previous: &Path) -> Result<(), String> {
    let aside = sibling(exe, ".rollback");
    let _ = fs::remove_file(&aside);
    fs::rename(exe, &aside).map_err(|e| format!("cannot roll back: {e}"))?;
    if let Err(e) = fs::rename(previous, exe) {
        let _ = fs::rename(&aside, exe);
        return Err(format!("cannot roll back: {e}"));
    }
    let _ = fs::rename(&aside, previous);
    Ok(())
}

#[cfg(not(windows))]
fn restore_previous(exe: &Path, previous: &Path) -> Result<(), String> {
    fs::rename(previous, exe).map_err(|e| format!("cannot roll back: {e}"))
}

// ---- Local-data retention (contract §4a) ---------------------------------
//
// Only local files an install leaves behind are removed here. Nothing in this
// section changes the check, its gates, its request or its cache format.

/// `<binary>.previous.json`: one version string and one Unix second.
fn rollback_sidecar(previous: &Path) -> PathBuf {
    let mut name = previous.as_os_str().to_os_string();
    name.push(".json");
    PathBuf::from(name)
}

/// Best effort: a sidecar that cannot be written only means the copy is kept,
/// since there is no trustworthy age to apply.
fn record_rollback(previous: &Path, version: &str, at: u64) {
    let path = rollback_sidecar(previous);
    let body = json!({ "version": version, "swapped_at": at }).to_string();
    if fs::write(&path, body).is_err() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
}

fn read_rollback_marker(path: &Path) -> Option<(String, u64)> {
    let value: Value = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    let version = value.get("version")?.as_str()?.to_string();
    let swapped_at = value.get("swapped_at")?.as_u64()?;
    Some((version, swapped_at))
}

fn modified_secs(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|age| age.as_secs())
}

fn expired(created: Option<u64>, at: u64, max_age: u64) -> bool {
    created
        .map(|when| at >= when.saturating_add(max_age))
        .unwrap_or(false)
}

/// Sweep the rollback copy and abandoned install leftovers after a successful
/// command. Returns the number of local files and directories removed.
pub fn sweep_local_data(current: &str) -> usize {
    let Ok(exe) = exe() else { return 0 };
    let paths = Paths::new();
    let cache = paths.cache.parent().unwrap_or_else(|| Path::new("."));
    sweep_at(&exe, cache, current, now())
}

fn sweep_at(target: &Path, cache: &Path, current: &str, at: u64) -> usize {
    let mut removed = 0;
    let previous = sibling(target, ".previous");
    let sidecar = rollback_sidecar(&previous);
    if let Some(parent) = target.parent() {
        removed += sweep_install_debris(parent, target, at);
    }
    if previous.is_file() {
        let marker = read_rollback_marker(&sidecar).or_else(|| {
            // No marker means no known install time. An upgraded copy or one
            // made by an older build gets a fresh clock from its first sight.
            record_rollback(&previous, ROLLBACK_OBSERVED_VERSION, at);
            read_rollback_marker(&sidecar)
        });
        let ran_ok = marker
            .as_ref()
            .is_some_and(|(version, _)| version == current);
        let installed_ok = read::<Cache>(&cache.join("update-check.json"))
            .installed
            .as_deref()
            == Some(current);
        let aged = marker
            .as_ref()
            .is_some_and(|(_, swapped)| expired(Some(*swapped), at, ROLLBACK_MAX_AGE_SECS));
        if (ran_ok || installed_ok || aged) && fs::remove_file(&previous).is_ok() {
            removed += 1;
        }
    }
    if !previous.exists() && fs::remove_file(&sidecar).is_ok() {
        removed += 1;
    }
    removed + sweep_cache_debris(cache, at)
}

/// Swap and rollback temp files left by an interrupted install are debris after
/// an hour. No check, cache or config state is touched.
fn sweep_install_debris(parent: &Path, target: &Path, at: u64) -> usize {
    let Ok(entries) = fs::read_dir(parent) else {
        return 0;
    };
    let base = target
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let swap_tmps = [format!("{base}.next"), format!("{base}.rollback")];
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !swap_tmps.iter().any(|tmp| tmp == &name) {
            continue;
        }
        if expired(modified_secs(&entry.path()), at, DEBRIS_MAX_AGE_SECS)
            && fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// Half-written state temp files (`<file>.<pid>.tmp`) expire after an hour.
/// The state files themselves and the cache folder stay.
fn sweep_cache_debris(cache: &Path, at: u64) -> usize {
    let mut removed = 0;
    if let Ok(entries) = fs::read_dir(cache) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".tmp") || !entry.path().is_file() {
                continue;
            }
            if expired(modified_secs(&entry.path()), at, DEBRIS_MAX_AGE_SECS)
                && fs::remove_file(entry.path()).is_ok()
            {
                removed += 1;
            }
        }
    }
    removed
}

// ---------------------------------------------------------------- command

fn ask(question: &str, default_yes: bool) -> bool {
    print!("{question} ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    if io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    match answer.trim().to_ascii_lowercase().as_str() {
        "" => default_yes,
        "y" | "yes" => true,
        _ => false,
    }
}

fn fail(cause: &str) -> i32 {
    eprintln!("ultracompress: {cause}");
    1
}

/// `ultracompress update [--yes] | --check | --skip | --auto on|off|status | --rollback | --clean`.
pub fn run(args: &[String]) -> i32 {
    let mut yes = false;
    let mut action: Option<(&str, &str)> = None;
    let mut iter = args.iter().map(String::as_str);
    while let Some(arg) = iter.next() {
        let next = match arg {
            "--yes" | "-y" => {
                yes = true;
                continue;
            }
            "--check" | "--skip" | "--rollback" | "--clean" | "--notice" => (arg, ""),
            "--auto" => (arg, iter.next().unwrap_or("")),
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            other => {
                eprintln!("ultracompress: unknown update option '{other}'\n{USAGE}");
                return 2;
            }
        };
        if action.replace(next).is_some() {
            eprintln!("ultracompress: one update action at a time\n{USAGE}");
            return 2;
        }
    }
    let paths = Paths::new();
    match action {
        None => update_now(yes, &paths),
        Some(("--check", _)) => report(&paths),
        Some(("--skip", _)) => skip(&paths),
        Some(("--auto", value)) => auto(value, &paths),
        Some(("--rollback", _)) => roll_back(&paths),
        Some(("--clean", _)) => clean(&paths),
        // One JSON line for agent host footers; `{}` when nothing is due.
        Some(_) => {
            let notice = notice_now().map_or(
                json!({}),
                |latest| json!({ "latest": latest, "current": VERSION }),
            );
            println!("{notice}");
            0
        }
    }
}

fn update_now(yes: bool, paths: &Paths) -> i32 {
    let release = match check_now(paths) {
        Ok(Some(release)) => release,
        Ok(None) => {
            println!("ultracompress {VERSION} is up to date.");
            return 0;
        }
        Err(cause) => return fail(&format!("update check failed: {cause}")),
    };
    let new = text(release.version);
    println!("ultracompress {VERSION} → {new}");
    for line in release.notes.lines() {
        println!("  {line}");
    }
    let exe = match exe() {
        Ok(exe) => exe,
        Err(cause) => return fail(&cause),
    };
    let tty = io::stdin().is_terminal() && io::stdout().is_terminal();
    if let Some((manager, command)) = managed(&exe, &new) {
        println!(
            "Installed with {manager}; updating with: {}",
            command.join(" ")
        );
        if tty && !yes && !ask("Run it now? [Y/n]", true) {
            return 0;
        }
        return match Command::new(&command[0]).args(&command[1..]).status() {
            Ok(status) if status.success() => 0,
            Ok(status) => fail(&format!("{} exited with {status}", command[0])),
            Err(e) => fail(&format!("cannot run {}: {e}", command[0])),
        };
    }
    if tty && !yes && !ask(&format!("Install {new} now? [Y/n]"), true) {
        return 0;
    }
    if let Err(cause) = install(&release, &exe, paths) {
        return fail(&cause);
    }
    println!("Installed ultracompress {new}. To go back: ultracompress update --rollback");
    let mut config: Config = read(&paths.config);
    if tty && !config.install_automatically && !config.asked_auto {
        config.asked_auto = true;
        config.install_automatically = ask("Install future updates automatically? [y/N]", false);
        if let Err(cause) = write(&paths.config, &config) {
            return fail(&cause);
        }
    }
    0
}

/// Report only: exit 0 up to date, 10 update available, 1 check failed.
fn report(paths: &Paths) -> i32 {
    let release = match check_now(paths) {
        Ok(Some(release)) => release,
        Ok(None) => {
            println!("ultracompress {VERSION} is up to date.");
            return 0;
        }
        Err(cause) => return fail(&format!("update check failed: {cause}")),
    };
    let new = text(release.version);
    let config: Config = read(&paths.config);
    let skipped = config.skip_version.as_deref().and_then(parse_version) >= Some(release.version);
    println!(
        "ultracompress {new} available (you have {VERSION}){}",
        if skipped { " · skipped" } else { "" }
    );
    if let Some((manager, command)) = exe().ok().and_then(|exe| managed(&exe, &new)) {
        println!(
            "Installed with {manager}; update with: {}",
            command.join(" ")
        );
    }
    let cache: Cache = read(&paths.cache);
    if let (Some(tried), Some(cause)) = (&cache.auto_tried, &cache.auto_error) {
        if *tried == new {
            println!("Automatic install failed: {cause}");
        }
    }
    10
}

fn skip(paths: &Paths) -> i32 {
    let cache: Cache = read(&paths.cache);
    let latest = if cache.error.is_none() && fresh(&cache, now()) {
        cache
            .latest
            .as_deref()
            .and_then(parse_version)
            .filter(|v| *v > current())
    } else {
        match check_now(paths) {
            Ok(release) => release.map(|r| r.version),
            Err(cause) => return fail(&format!("update check failed: {cause}")),
        }
    };
    let Some(latest) = latest.map(text) else {
        println!("ultracompress {VERSION} is up to date; nothing to skip.");
        return 0;
    };
    let mut config: Config = read(&paths.config);
    config.skip_version = Some(latest.clone());
    if let Err(cause) = write(&paths.config, &config) {
        return fail(&cause);
    }
    println!("Skipped {latest}. A newer release will notify again.");
    0
}

fn auto(value: &str, paths: &Paths) -> i32 {
    let mut config: Config = read(&paths.config);
    match value {
        "status" => {}
        "on" | "off" => {
            config.install_automatically = value == "on";
            config.asked_auto = true;
            if let Err(cause) = write(&paths.config, &config) {
                return fail(&cause);
            }
            if value == "off" {
                let _ = fs::remove_file(&paths.log);
            }
        }
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    }
    let state = if config.install_automatically {
        "on"
    } else {
        "off"
    };
    println!("Automatic updates: {state}");
    0
}

fn roll_back(paths: &Paths) -> i32 {
    let result = exe().and_then(|exe| rollback(&exe));
    if let Err(cause) = result {
        return fail(&cause);
    }
    // The version rolled back from is skipped, so it is neither announced nor reinstalled.
    let mut config: Config = read(&paths.config);
    config.skip_version = Some(VERSION.to_string());
    let _ = write(&paths.config, &config);
    println!(
        "Restored the previous binary. {VERSION} is skipped; a newer release will notify again."
    );
    0
}

/// Uninstall helper: removes the update cache, settings, log and rollback copy.
fn clean(paths: &Paths) -> i32 {
    let previous = exe().ok().map(|exe| sibling(&exe, ".previous"));
    let sidecar = previous.as_ref().map(|path| rollback_sidecar(path));
    for path in [&paths.cache, &paths.config, &paths.log]
        .into_iter()
        .chain(previous.as_ref())
        .chain(sidecar.as_ref())
    {
        let _ = fs::remove_file(path);
        if let Some(dir) = path.parent() {
            if dir.file_name().is_some_and(|name| name == "ultracompress") {
                let _ = fs::remove_dir(dir); // only when empty
            }
        }
    }
    println!("Removed the update cache, settings and rollback copy.");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("uc-update-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cache(latest: &str, checked_at: u64) -> Cache {
        Cache {
            checked_at,
            latest: Some(latest.into()),
            ..Cache::default()
        }
    }

    #[test]
    fn versions_newer_equal_older_garbage_prerelease() {
        assert_eq!(parse_version("v0.8.0"), Some((0, 8, 0)));
        assert_eq!(parse_version(" 1.12.3 "), Some((1, 12, 3)));
        for bad in [
            "",
            "v",
            "1.2",
            "1.2.3.4",
            "v1.2.3-rc.1",
            "1.2.x",
            "1..3",
            "nightly",
            "v-1.2.3",
        ] {
            assert_eq!(parse_version(bad), None, "{bad}");
        }
        let current = (0, 8, 0);
        let newer = |tag: &str| pending(&cache(tag, 1), &Config::default(), current).is_some();
        assert!(newer("0.8.1") && newer("0.10.0") && newer("1.0.0"));
        assert!(!newer("0.8.0") && !newer("0.7.9") && !newer("garbage") && !newer("0.9.0-rc1"));
    }

    #[test]
    fn release_records_drop_drafts_prereleases_and_bad_tags() {
        let release = json!({"tag_name": "v0.9.0", "body": "a\n\n b \nc\nd\ne\nf\ng",
            "assets": [{"name": "SHA256SUMS", "browser_download_url": "https://example.test/S"}]});
        let found = parse_release(&release).unwrap().unwrap();
        assert_eq!(found.version, (0, 9, 0));
        assert_eq!(found.notes, "a\n b\nc\nd\ne\nf");
        assert_eq!(found.url("SHA256SUMS").unwrap(), "https://example.test/S");
        assert!(found.url("missing.zip").is_err());
        for skipped in [
            json!({"tag_name": "v0.9.0", "draft": true}),
            json!({"tag_name": "v0.9.0", "prerelease": true}),
            json!({"tag_name": "v0.9.0-beta.1"}),
            json!({"name": "no tag"}),
        ] {
            assert!(parse_release(&skipped).unwrap().is_none(), "{skipped}");
        }
        assert!(parse_release(&json!(["not", "an", "object"])).is_err());
        let long = "x".repeat(NOTES_MAX_CHARS + 50);
        assert_eq!(first_lines(&long).chars().count(), NOTES_MAX_CHARS + 1);
    }

    #[test]
    fn check_runs_once_a_day_and_keeps_failure_causes() {
        let mut state = Cache::default();
        assert!(plan(&state, &Config::default(), (0, 8, 0), 1_000).refresh);
        record(&mut state, &Err("api.github.com: timed out".into()), 1_000);
        assert_eq!(state.error.as_deref(), Some("api.github.com: timed out"));
        assert!(!plan(&state, &Config::default(), (0, 8, 0), 1_000 + DAY - 1).refresh);
        assert!(plan(&state, &Config::default(), (0, 8, 0), 1_000 + DAY).refresh);
        // A clock set back does not freeze the check.
        assert!(plan(&state, &Config::default(), (0, 8, 0), 10).refresh);
        // A failure keeps the last known release; a success clears the cause.
        let mut known = cache("0.9.0", 1);
        record(&mut known, &Err("offline".into()), 2);
        assert_eq!(known.latest.as_deref(), Some("0.9.0"));
        record(&mut known, &Ok(None), 3);
        assert_eq!((known.latest, known.error), (None, None));
    }

    #[test]
    fn notice_once_a_day_per_version_and_skip_hides_one_version() {
        let config = Config::default();
        let mut state = cache("0.9.0", 100);
        let first = plan(&state, &config, (0, 8, 0), 200);
        assert_eq!(first.notice.as_deref(), Some("0.9.0"));
        assert!(!first.refresh && first.install.is_none());
        state.noticed = Some("0.9.0".into());
        state.noticed_at = 200;
        assert_eq!(plan(&state, &config, (0, 8, 0), 300).notice, None);
        assert!(plan(&state, &config, (0, 8, 0), 200 + DAY).notice.is_some());
        state.latest = Some("0.9.1".into());
        assert!(plan(&state, &config, (0, 8, 0), 300).notice.is_some());

        let skipped = Config {
            skip_version: Some("0.9.1".into()),
            ..Config::default()
        };
        assert_eq!(plan(&state, &skipped, (0, 8, 0), 300).notice, None);
        state.latest = Some("0.10.0".into());
        assert_eq!(
            plan(&state, &skipped, (0, 8, 0), 300).notice.as_deref(),
            Some("0.10.0")
        );
    }

    #[test]
    fn auto_is_off_by_default_installs_once_and_falls_back_to_the_notice() {
        assert!(!Config::default().install_automatically);
        assert!(Config::default().check_automatically);
        let auto = Config {
            install_automatically: true,
            ..Config::default()
        };
        let mut state = cache("0.9.0", 100);
        let start = plan(&state, &auto, (0, 8, 0), 200);
        assert_eq!(
            (start.install.as_deref(), start.notice),
            (Some("0.9.0"), None)
        );
        state.auto_tried = Some("0.9.0".into());
        assert_eq!(plan(&state, &auto, (0, 8, 0), 200), Plan::default());
        state.auto_error = Some("checksum mismatch".into());
        let fallback = plan(&state, &auto, (0, 8, 0), 200);
        assert_eq!(
            (fallback.install, fallback.notice.as_deref()),
            (None, Some("0.9.0"))
        );
    }

    #[test]
    fn opt_outs_ci_and_claude_code_are_quiet() {
        let only = |key: &'static str, value: &'static str| {
            move |k: &str| (k == key).then(|| value.to_string())
        };
        assert!(quiet(only("ULTRACOMPRESS_NO_UPDATE_CHECK", "1")));
        assert!(quiet(only("CI", "true")));
        assert!(quiet(only("CLAUDECODE", "1")));
        assert!(!quiet(only("ULTRACOMPRESS_NO_UPDATE_CHECK", "0")));
        assert!(!quiet(|_| None));
    }

    #[test]
    fn notice_text_is_two_short_lines() {
        assert_eq!(
            notice_lines("0.9.0", "0.8.0"),
            [
                "ultracompress 0.9.0 available (you have 0.8.0) · run: ultracompress update",
                "turn on automatic updates: ultracompress update --auto on · skip: ultracompress update --skip",
            ]
        );
    }

    #[test]
    fn state_is_atomic_owner_only_and_persists_choices() {
        let dir = temp("state");
        let path = dir.join("nested").join("config.json");
        let config = Config {
            install_automatically: true,
            skip_version: Some("0.9.0".into()),
            ..Config::default()
        };
        write(&path, &config).unwrap();
        assert_eq!(read::<Config>(&path), config);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        fs::write(&path, "{broken").unwrap();
        assert_eq!(read::<Config>(&path), Config::default());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn log_stays_under_256_kb() {
        let dir = temp("log");
        let path = dir.join("update.log");
        fs::write(&path, vec![b'x'; LOG_MAX_BYTES as usize - 8]).unwrap();
        log(&path, "installing 0.9.0");
        let size = fs::metadata(&path).unwrap().len();
        assert!(size < 64, "{size}");
        log(&path, "installed 0.9.0");
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn swap_keeps_one_previous_and_rollback_restores_it() {
        let dir = temp("swap");
        let exe = dir.join("ultracompress");
        let staged = dir.join("staged");
        fs::write(&exe, "v1").unwrap();
        fs::write(&staged, "v2").unwrap();
        swap(&exe, &staged).unwrap();
        fs::write(&staged, "v3").unwrap();
        swap(&exe, &staged).unwrap();
        assert_eq!(fs::read_to_string(&exe).unwrap(), "v3");
        assert_eq!(
            fs::read_to_string(sibling(&exe, ".previous")).unwrap(),
            "v2"
        );
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 3, "{names:?}"); // exe, .previous, staged
        rollback(&exe).unwrap();
        assert_eq!(fs::read_to_string(&exe).unwrap(), "v2");
        assert!(!sibling(&exe, ".previous").exists());
        assert!(rollback(&exe).unwrap_err().contains("nothing to roll back"));
        // A missing download changes nothing.
        assert!(swap(&exe, &dir.join("absent")).is_err());
        assert_eq!(fs::read_to_string(&exe).unwrap(), "v2");
        assert!(!sibling(&exe, ".next").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn checksums_and_managed_installs() {
        let hash = "AB".repeat(32);
        let sums = format!("{hash}  tool.zip\n{}  *other.tar.gz\n", "c".repeat(64));
        assert_eq!(
            checksum_for(&sums, "tool.zip").unwrap(),
            hash.to_lowercase()
        );
        assert_eq!(checksum_for(&sums, "other.tar.gz").unwrap(), "c".repeat(64));
        assert!(checksum_for(&sums, "missing.zip").is_err());
        let brew = Path::new("/opt/homebrew/Cellar/ultracompress/0.8.0/bin/ultracompress");
        assert_eq!(managed(brew, "0.9.0").unwrap().0, "Homebrew");
        let (manager, command) =
            managed(Path::new("/h/.cargo/bin/ultracompress"), "0.9.0").unwrap();
        assert_eq!(manager, "Cargo");
        assert!(command.join(" ").contains("--tag v0.9.0 ultracompress"));
        assert!(managed(Path::new("/h/.ultracompress/ultracompress"), "0.9.0").is_none());
    }

    #[test]
    fn paths_follow_the_platform_and_overrides() {
        let home = |key: &str| (key == "HOME").then(|| PathBuf::from("/h"));
        let paths = Paths::from_env(home);
        let expected = if cfg!(target_os = "macos") {
            "/h/Library/Caches/ultracompress/update-check.json"
        } else {
            "/h/.cache/ultracompress/update-check.json"
        };
        if cfg!(unix) {
            assert_eq!(paths.cache, PathBuf::from(expected));
        }
        let paths = Paths::from_env(|key| match key {
            "ULTRACOMPRESS_CACHE_DIR" => Some(PathBuf::from("/c")),
            "ULTRACOMPRESS_CONFIG_DIR" => Some(PathBuf::from("/s")),
            _ => None,
        });
        assert_eq!(paths.cache, PathBuf::from("/c/update-check.json"));
        assert_eq!(paths.log, PathBuf::from("/c/update.log"));
        assert_eq!(paths.config, PathBuf::from("/s/config.json"));
    }

    fn set_mtime(path: &Path, secs: u64) {
        let file = fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .unwrap();
    }

    #[test]
    fn rollback_copy_goes_away_once_the_replacement_runs() {
        let root = temp("sweep-ran");
        let cache = root.join("cache");
        fs::create_dir_all(&cache).unwrap();
        let target = root.join("ultracompress");
        let previous = sibling(&target, ".previous");
        fs::write(&target, "new").unwrap();
        fs::write(&previous, "old").unwrap();
        record_rollback(&previous, "9.9.9", 1_000);
        assert!(rollback_sidecar(&previous).exists());
        assert_eq!(sweep_at(&target, &cache, "9.9.9", 1_060), 2);
        assert!(!previous.exists());
        assert!(!rollback_sidecar(&previous).exists());
        assert!(target.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rollback_copy_survives_until_a_successful_run_or_seven_days() {
        let root = temp("sweep-wait");
        let cache = root.join("cache");
        fs::create_dir_all(&cache).unwrap();
        let target = root.join("ultracompress");
        let previous = sibling(&target, ".previous");
        fs::write(&target, "new").unwrap();
        fs::write(&previous, "old").unwrap();
        record_rollback(&previous, "9.9.9", 1_000);
        assert_eq!(sweep_at(&target, &cache, "9.9.8", 1_060), 0);
        assert!(previous.exists());
        assert_eq!(
            sweep_at(&target, &cache, "9.9.8", 1_000 + ROLLBACK_MAX_AGE_SECS),
            2
        );
        assert!(!previous.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn copy_without_a_sidecar_is_not_aged_by_the_old_binary_mtime() {
        let root = temp("sweep-observed");
        let cache = root.join("cache");
        fs::create_dir_all(&cache).unwrap();
        let target = root.join("ultracompress");
        let previous = sibling(&target, ".previous");
        fs::write(&target, "new").unwrap();
        fs::write(&previous, "old").unwrap();
        set_mtime(&previous, 1); // an ancient binary, not an install time
        let observed_at = 10_000 + ROLLBACK_MAX_AGE_SECS;
        assert_eq!(sweep_at(&target, &cache, "9.9.8", observed_at), 0);
        assert!(previous.exists());
        assert_eq!(
            read_rollback_marker(&rollback_sidecar(&previous)),
            Some((ROLLBACK_OBSERVED_VERSION.to_string(), observed_at))
        );
        assert_eq!(
            sweep_at(
                &target,
                &cache,
                "9.9.8",
                observed_at + ROLLBACK_MAX_AGE_SECS - 1
            ),
            0
        );
        assert_eq!(
            sweep_at(
                &target,
                &cache,
                "9.9.8",
                observed_at + ROLLBACK_MAX_AGE_SECS
            ),
            2
        );
        assert!(!previous.exists());
        assert!(!rollback_sidecar(&previous).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interrupted_installs_leave_no_debris() {
        let root = temp("sweep-debris");
        let cache = root.join("cache");
        fs::create_dir_all(&cache).unwrap();
        let target = root.join("ultracompress");
        fs::write(&target, "new").unwrap();
        let old = 2_000u64;
        let swap_tmp = sibling(&target, ".next");
        let state_tmp = cache.join("update-check.json.1234.tmp");
        let state = cache.join("update-check.json");
        let foreign = root.join("notes.next");
        fs::write(&swap_tmp, "half-written").unwrap();
        fs::write(&state_tmp, "partial write").unwrap();
        fs::write(&state, "{}").unwrap();
        fs::write(&foreign, "not ours").unwrap();
        for path in [&swap_tmp, &state_tmp, &state, &foreign] {
            set_mtime(path, old);
        }
        assert_eq!(sweep_at(&target, &cache, "9.9.9", old + 60), 0);
        assert!(swap_tmp.exists() && state_tmp.exists());
        let later = old + DEBRIS_MAX_AGE_SECS + 1;
        assert_eq!(sweep_at(&target, &cache, "9.9.9", later), 2);
        assert!(!swap_tmp.exists());
        assert!(!state_tmp.exists());
        assert!(state.exists() && foreign.exists() && cache.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
