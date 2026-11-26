//! Update-contract state and decisions (no network, no host APIs).
//!
//! The CLI owns the one anonymous HTTP request; this module owns the rules:
//! stable-version comparison, the at-most-daily cache, opt-out/CI/agent gates,
//! skip and notice frequency, and private atomic persistence. Keeping the
//! decisions here makes them testable without a network or a terminal.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// One check per 24 h per machine, and one notice per 24 h per version.
pub const CHECK_INTERVAL_SECS: u64 = 86_400;
/// The single cache file name used by the CLI, Pi and Hermes.
pub const CACHE_FILE: &str = "update.json";
/// Hard ceiling for the anonymous request (contract: at most four seconds).
pub const CHECK_TIMEOUT_SECS: u64 = 4;
/// Fixed, anonymous user agent: no id, version, usage or machine data.
pub const USER_AGENT: &str = "ultracompress-update-check";
pub const OPT_OUT_ENV: &str = "ULTRACOMPRESS_NO_UPDATE_CHECK";

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// Only stable `x.y.z` (optionally prefixed by `v`) is a release. Drafts,
    /// prereleases, build metadata and unparseable tags are not newer.
    pub fn parse(text: &str) -> Option<Self> {
        let body = text.strip_prefix('v').unwrap_or(text);
        if body.is_empty() || !body.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            return None;
        }
        let mut parts = body.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        if parts.next().is_some() || body.matches('.').count() != 2 {
            return None;
        }
        Some(Version {
            major,
            minor,
            patch,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// First lines of the release notes, bounded for a short notice/report.
    pub notes: String,
}

/// Parse the public GitHub "latest release" record. Drafts and prereleases are
/// ignored even when GitHub reports them as latest.
pub fn release_from_json(body: &str) -> Result<Release, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid release response: {e}"))?;
    let obj = value
        .as_object()
        .ok_or_else(|| "release response is not an object".to_string())?;
    if obj.get("draft") == Some(&serde_json::Value::Bool(true)) {
        return Err("latest release is a draft; ignored".to_string());
    }
    if obj.get("prerelease") == Some(&serde_json::Value::Bool(true)) {
        return Err("latest release is a prerelease; ignored".to_string());
    }
    let tag = obj
        .get("tag_name")
        .and_then(|t| t.as_str())
        .ok_or_else(|| "release has no tag_name".to_string())?;
    let version = Version::parse(tag)
        .ok_or_else(|| format!("release tag '{tag}' is not a stable x.y.z version"))?;
    let notes = obj
        .get("body")
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(5)
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .take(600)
        .collect();
    Ok(Release {
        version: version.to_string(),
        notes,
    })
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    match (Version::parse(latest), Version::parse(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

/// Derive the one allowed source from the package's own repository metadata.
/// No person or project name is hard-coded here.
pub fn latest_release_url(repository: &str) -> Result<String, String> {
    let rest = repository
        .strip_prefix("https://github.com/")
        .ok_or_else(|| format!("repository metadata is not a GitHub URL: {repository}"))?;
    let slug = rest.trim_end_matches('/').trim_end_matches(".git");
    let (owner, repo) = slug
        .split_once('/')
        .ok_or_else(|| format!("repository metadata has no owner/repo: {repository}"))?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return Err(format!(
            "repository metadata has no owner/repo: {repository}"
        ));
    }
    Ok(format!(
        "https://api.github.com/repos/{owner}/{repo}/releases/latest"
    ))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateCache {
    /// Unix seconds of the last completed attempt, success or failure.
    pub checked_at: u64,
    /// Cause of the last failed check; shown by `update --check`, never as noise.
    pub last_error: Option<String>,
    pub release: Option<Release>,
    pub skipped_version: Option<String>,
    pub shown_version: Option<String>,
    pub shown_at: u64,
    /// Auto-install is attempted once per version, never in a retry loop.
    #[serde(default)]
    pub auto_attempted_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateConfig {
    pub check_enabled: bool,
    /// Opt-in only; never enabled by a background check or an env var.
    pub auto_install: bool,
    /// Remember a decline so the question is never asked twice.
    pub auto_asked: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        UpdateConfig {
            check_enabled: true,
            auto_install: false,
            auto_asked: false,
        }
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The same platform cache root is used by the CLI, Pi and Hermes so the one
/// daily check is shared by all three adapters on a machine. Overrides are
/// read in the same order: `XDG_CACHE_HOME`, Windows `LOCALAPPDATA`, macOS
/// `~/Library/Caches`, then `~/.cache` (with `USERPROFILE` on Windows).
pub fn cache_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("ultracompress");
        }
    }
    #[cfg(windows)]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            if !local.is_empty() {
                return PathBuf::from(local).join("ultracompress");
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if !home.is_empty() {
            return PathBuf::from(home).join("Library/Caches/ultracompress");
        }
    }
    if !home.is_empty() {
        return PathBuf::from(home).join(".cache/ultracompress");
    }
    PathBuf::from("ultracompress-update-cache")
}

pub fn config_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("ultracompress");
        }
    }
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            if !appdata.is_empty() {
                return PathBuf::from(appdata).join("ultracompress");
            }
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() {
        return PathBuf::from(home).join(".config/ultracompress");
    }
    PathBuf::from("ultracompress-update-config")
}

pub fn cache_path() -> PathBuf {
    cache_dir().join(CACHE_FILE)
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

/// Atomic, owner-only write. A partial file is never visible to another process.
pub fn write_private_json<T: Serialize>(path: &PathBuf, value: &T) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| "no parent directory for state file".to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let stamp = now_secs();
    let tmp = dir.join(format!(
        ".{}.{}-{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("state"),
        std::process::id(),
        stamp
    ));
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, &bytes).map_err(|e| format!("cannot write state: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("cannot protect state: {e}"))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot publish state: {e}")
    })
}

fn read_json<T: serde::de::DeserializeOwned + Default>(path: &PathBuf) -> T {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<T>(&bytes).ok())
        .unwrap_or_default()
}

/// The one on-disk schema: snake_case keys and integer Unix seconds on all
/// adapters. A pre-0.6.8 Pi cache used camelCase keys with milliseconds; it is
/// converted here so the daily interval does not change meaning after an
/// upgrade. A Hermes cache with fractional seconds is truncated, not treated
/// as milliseconds.
pub fn parse_cache(body: &str) -> UpdateCache {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return UpdateCache::default();
    };
    let Some(obj) = value.as_object() else {
        return UpdateCache::default();
    };
    let stamp = |key: &str, legacy: &str| -> u64 {
        let legacy_unit = !obj.contains_key(key) && obj.contains_key(legacy);
        let raw = obj.get(key).or_else(|| obj.get(legacy));
        match raw.and_then(|value| value.as_f64()) {
            Some(number) if number.is_finite() && number > 0.0 => {
                let seconds = if legacy_unit { number / 1000.0 } else { number };
                seconds as u64
            }
            _ => 0,
        }
    };
    let text = |key: &str, legacy: &str| -> Option<String> {
        obj.get(key)
            .or_else(|| obj.get(legacy))
            .and_then(|value| value.as_str())
            .map(str::to_string)
    };
    let release = obj
        .get("release")
        .cloned()
        .and_then(|value| serde_json::from_value::<Release>(value).ok());
    UpdateCache {
        checked_at: stamp("checked_at", "checkedAt"),
        last_error: text("last_error", "lastError"),
        release,
        skipped_version: text("skipped_version", "skippedVersion"),
        shown_version: text("shown_version", "shownVersion"),
        shown_at: stamp("shown_at", "shownAt"),
        auto_attempted_version: text("auto_attempted_version", "autoAttemptedVersion"),
    }
}

pub fn read_cache() -> UpdateCache {
    match std::fs::read_to_string(cache_path()) {
        Ok(body) => parse_cache(&body),
        Err(_) => UpdateCache::default(),
    }
}

pub fn read_config() -> UpdateConfig {
    read_json(&config_path())
}

pub fn write_cache(cache: &UpdateCache) -> Result<(), String> {
    write_private_json(&cache_path(), cache)
}

pub fn write_config(config: &UpdateConfig) -> Result<(), String> {
    write_private_json(&config_path(), config)
}

pub fn check_due(cache: &UpdateCache, now: u64) -> bool {
    cache.checked_at == 0 || now.saturating_sub(cache.checked_at) >= CHECK_INTERVAL_SECS
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Gate {
    Allowed,
    OptOut,
    NonInteractive,
    Ci,
    AgentMode,
    DisabledInConfig,
}

impl fmt::Display for Gate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Gate::Allowed => "allowed",
            Gate::OptOut => "disabled by ULTRACOMPRESS_NO_UPDATE_CHECK=1",
            Gate::NonInteractive => "not an interactive terminal",
            Gate::Ci => "CI environment",
            Gate::AgentMode => "agent/headless mode",
            Gate::DisabledInConfig => "disabled in config",
        };
        f.write_str(text)
    }
}

/// `interactive` comes from the host (TTY for the CLI, live UI for adapters).
/// `explicit` is an update command the user just typed; it is allowed on a
/// non-TTY because the command itself is consent, but never in CI/agent mode
/// or when the user opted out.
pub fn gate(interactive: bool, explicit: bool, config: &UpdateConfig) -> Gate {
    if std::env::var(OPT_OUT_ENV)
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return Gate::OptOut;
    }
    if !config.check_enabled {
        return Gate::DisabledInConfig;
    }
    if std::env::var("CI").map(|v| !v.is_empty()).unwrap_or(false) {
        return Gate::Ci;
    }
    if std::env::var("ULTRACOMPRESS_AGENT_MODE")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return Gate::AgentMode;
    }
    if !interactive && !explicit {
        Gate::NonInteractive
    } else {
        Gate::Allowed
    }
}

/// A newer version is shown at most once per 24 h, never if skipped. Callers
/// pass their runtime version so adapters and tests can use fixtures.
pub fn showable(cache: &UpdateCache, release: &Release, current: &str, now: u64) -> bool {
    if !is_newer(&release.version, current) {
        return false;
    }
    if cache.skipped_version.as_deref() == Some(release.version.as_str()) {
        return false;
    }
    if cache.shown_version.as_deref() == Some(release.version.as_str())
        && now.saturating_sub(cache.shown_at) < CHECK_INTERVAL_SECS
    {
        return false;
    }
    true
}

pub fn mark_shown(cache: &mut UpdateCache, version: &str, now: u64) {
    cache.shown_version = Some(version.to_string());
    cache.shown_at = now;
}

pub fn skip(cache: &mut UpdateCache, version: Option<&str>) {
    cache.skipped_version = version.map(str::to_string);
}

/// Two lines on stderr after the command finishes, never in JSON on stdout.
pub fn notice(release: &Release, current: &str) -> String {
    format!(
        "ultracompress {} available (you have {}) · run: ultracompress update\n\
         turn on automatic updates: ultracompress update --auto on · skip: ultracompress update --skip",
        release.version, current
    )
}

pub fn check_report(
    release: Option<&Release>,
    error: Option<&str>,
    current: &str,
) -> (String, i32) {
    if let Some(release) = release {
        if is_newer(&release.version, current) {
            return (
                format!(
                    "ultracompress {} available (you have {}) · run: ultracompress update",
                    release.version, current
                ),
                10,
            );
        }
        return (format!("ultracompress is up to date ({current})"), 0);
    }
    match error {
        Some(cause) => (format!("ultracompress update check failed: {cause}"), 1),
        None => (format!("ultracompress is up to date ({current})"), 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison_is_strict_and_stable() {
        assert!(is_newer("0.6.7", "0.6.6"));
        assert!(is_newer("0.7.0", "0.6.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.6.6", "0.6.6"));
        assert!(!is_newer("0.6.5", "0.6.6"));
        assert!(!is_newer("garbage", "0.6.6"));
        assert!(!is_newer("0.6.7-beta.1", "0.6.6"));
        assert!(!is_newer("0.6.6+build.4", "0.6.6"));
        assert_eq!(
            Version::parse("v0.6.7"),
            Some(Version {
                major: 0,
                minor: 6,
                patch: 7
            })
        );
        assert_eq!(Version::parse("0.6"), None);
        assert_eq!(Version::parse("0.6.7.1"), None);
    }

    #[test]
    fn release_source_comes_from_repository_metadata() {
        assert_eq!(
            latest_release_url("https://github.com/michael-berardi/ultracompress").unwrap(),
            "https://api.github.com/repos/michael-berardi/ultracompress/releases/latest"
        );
        assert!(latest_release_url("gitlab.com/o/r").is_err());
        assert!(latest_release_url("not a url").is_err());
    }

    #[test]
    fn only_stable_releases_are_selected() {
        let stable = r#"{"tag_name":"v0.6.7","draft":false,"prerelease":false,"body":"One\n\nTwo\nThree\nFour\nFive\nSix"}"#;
        let release = release_from_json(stable).unwrap();
        assert_eq!(release.version, "0.6.7");
        assert_eq!(release.notes, "One\nTwo\nThree\nFour\nFive");
        assert!(release_from_json(r#"{"tag_name":"v0.6.8","prerelease":true}"#).is_err());
        assert!(release_from_json(r#"{"tag_name":"v0.6.8","draft":true}"#).is_err());
        assert!(release_from_json(r#"{"tag_name":"nightly"}"#).is_err());
        assert!(release_from_json("not json").is_err());
    }

    #[test]
    fn cache_interval_skips_and_notice_frequency() {
        let release = Release {
            version: "0.6.7".into(),
            notes: "notes".into(),
        };
        let mut cache = UpdateCache::default();
        let now = 1_700_000_000;
        assert!(check_due(&cache, now));
        cache.checked_at = now;
        cache.release = Some(release.clone());
        assert!(!check_due(&cache, now + CHECK_INTERVAL_SECS - 1));
        assert!(check_due(&cache, now + CHECK_INTERVAL_SECS));
        assert!(showable(&cache, &release, "0.6.6", now));
        mark_shown(&mut cache, &release.version, now);
        assert!(!showable(&cache, &release, "0.6.6", now + 10));
        assert!(showable(
            &cache,
            &release,
            "0.6.6",
            now + CHECK_INTERVAL_SECS
        ));
        skip(&mut cache, Some(&release.version));
        assert!(!showable(
            &cache,
            &release,
            "0.6.6",
            now + CHECK_INTERVAL_SECS * 2
        ));
        let newer = Release {
            version: "0.6.8".into(),
            notes: "notes".into(),
        };
        assert!(showable(
            &cache,
            &newer,
            "0.6.6",
            now + CHECK_INTERVAL_SECS * 2
        ));
        assert!(!showable(&cache, &release, "0.6.7", now));
    }

    #[test]
    fn gate_blocks_ci_agent_and_opt_out() {
        let config = UpdateConfig::default();
        // The ambient test environment may set CI; only assert env-independent rules here.
        assert!(matches!(
            gate(false, false, &config),
            Gate::NonInteractive | Gate::Ci | Gate::AgentMode | Gate::OptOut
        ));
        assert!(matches!(
            gate(true, false, &config),
            Gate::Allowed | Gate::Ci | Gate::AgentMode | Gate::OptOut
        ));
        let disabled = UpdateConfig {
            check_enabled: false,
            ..UpdateConfig::default()
        };
        assert!(matches!(
            gate(true, true, &disabled),
            Gate::DisabledInConfig | Gate::Ci | Gate::AgentMode | Gate::OptOut
        ));
    }

    #[test]
    fn failure_is_cached_with_its_cause() {
        let cache = UpdateCache {
            checked_at: 42,
            last_error: Some("HTTP 503".into()),
            ..UpdateCache::default()
        };
        assert!(!check_due(&cache, 43));
        let (text, code) = check_report(None, cache.last_error.as_deref(), "0.6.6");
        assert_eq!(code, 1);
        assert!(text.contains("HTTP 503"));
    }

    #[test]
    fn notice_and_report_never_include_sensitive_data() {
        let release = Release {
            version: "0.6.7".into(),
            notes: "notes".into(),
        };
        let text = notice(&release, "0.6.6");
        assert!(text.contains("ultracompress 0.6.7 available (you have 0.6.6)"));
        assert!(text.contains("update --auto on"));
        assert!(text.contains("update --skip"));
        let (text, code) = check_report(Some(&release), None, "0.6.6");
        assert_eq!(code, 10);
        assert!(!text.contains("notes")); // report only, release notes come from `update`
    }

    #[test]
    fn cache_schema_uses_snake_case_unix_seconds() {
        let canonical = r#"{"checked_at":1700000000,"shown_at":1700000000,
            "release":{"version":"9.9.9","notes":"notes"},
            "skipped_version":null,"last_error":null,"auto_attempted_version":null}"#;
        let cache = parse_cache(canonical);
        assert_eq!(cache.checked_at, 1_700_000_000);
        assert_eq!(cache.shown_at, 1_700_000_000);
        assert_eq!(cache.release.as_ref().unwrap().version, "9.9.9");
        let written = serde_json::to_string(&cache).unwrap();
        assert!(written.contains("\"checked_at\""));
        assert!(!written.contains("checkedAt"));
        assert!(!check_due(&cache, 1_700_000_000 + CHECK_INTERVAL_SECS - 1));
        assert!(check_due(&cache, 1_700_000_000 + CHECK_INTERVAL_SECS));
    }

    #[test]
    fn legacy_pi_milliseconds_are_converted_on_read() {
        let legacy = r#"{"checkedAt":1700000000000,"shownAt":1700000000000,
            "release":{"version":"9.9.9","notes":"notes"},
            "lastError":"HTTP 503","shownVersion":"9.9.9"}"#;
        let cache = parse_cache(legacy);
        assert_eq!(cache.checked_at, 1_700_000_000);
        assert_eq!(cache.shown_at, 1_700_000_000);
        assert_eq!(cache.last_error.as_deref(), Some("HTTP 503"));
        assert_eq!(cache.shown_version.as_deref(), Some("9.9.9"));
        assert!(!check_due(&cache, 1_700_000_000 + CHECK_INTERVAL_SECS - 1));
        assert_eq!(
            serde_json::to_string(&cache)
                .unwrap()
                .matches("checked_at")
                .count(),
            1
        );
    }

    #[test]
    fn private_atomic_write_round_trips() {
        let dir = std::env::temp_dir().join(format!("uc-update-test-{}", std::process::id()));
        let path = dir.join("nested").join("update.json");
        let cache = UpdateCache {
            checked_at: 7,
            last_error: None,
            release: None,
            skipped_version: Some("0.6.7".into()),
            shown_version: None,
            shown_at: 0,
            auto_attempted_version: None,
        };
        write_private_json(&path, &cache).unwrap();
        let got: UpdateCache = read_json(&path);
        assert_eq!(got.checked_at, 7);
        assert_eq!(got.skipped_version.as_deref(), Some("0.6.7"));
        assert!(!dir.join("nested").read_dir().unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
