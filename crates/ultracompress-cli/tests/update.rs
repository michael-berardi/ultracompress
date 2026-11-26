//! `ultracompress update` end to end against a loopback fake release; no
//! external network. Each test runs its own copy of the binary, so an update
//! replaces that copy and never the binary cargo built.
#![cfg(unix)]

use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NEW: &str = "9.9.9";
const NEW_BINARY: &str = "#!/bin/sh\necho 'ultracompress 9.9.9'\n";

fn asset() -> &'static str {
    if cfg!(target_os = "macos") {
        "UltraCompress-macos-universal.zip"
    } else if cfg!(target_arch = "aarch64") {
        "ultracompress-linux-arm64.tar.gz"
    } else {
        "ultracompress-linux-x86_64.tar.gz"
    }
}

struct Fake {
    root: PathBuf,
    api: String,
    hits: Arc<AtomicUsize>,
    original: Vec<u8>,
}

impl Fake {
    /// A fake `releases/latest` for `tag`, serving the release archive and its SHA256SUMS.
    fn new(name: &str, tag: &'static str, prerelease: bool, good_sum: bool) -> Self {
        let root =
            std::env::temp_dir().join(format!("uc-update-e2e-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for dir in ["bin", "tmp", "home", "stage/cli"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        let exe = root.join("bin/ultracompress");
        fs::copy(env!("CARGO_BIN_EXE_ultracompress"), &exe).unwrap();
        let archive = build_archive(&root);
        let sum = if good_sum {
            format!("{:x}", Sha256::digest(&archive))
        } else {
            "0".repeat(64)
        };
        let sums = format!("{sum}  {}\n", asset());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let path = request_path(&mut stream);
                let base = format!("http://127.0.0.1:{port}/dl");
                let (status, body) = if path == "/release.json" {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let record = serde_json::json!({
                        "tag_name": tag, "draft": false, "prerelease": prerelease,
                        "body": "Fixes\n\n- one\n- two",
                        "assets": [
                            {"name": asset(), "browser_download_url": format!("{base}/{}", asset())},
                            {"name": "SHA256SUMS", "browser_download_url": format!("{base}/SHA256SUMS")}
                        ]
                    });
                    ("200 OK", record.to_string().into_bytes())
                } else if path == format!("/dl/{}", asset()) {
                    ("200 OK", archive.clone())
                } else if path == "/dl/SHA256SUMS" {
                    ("200 OK", sums.clone().into_bytes())
                } else {
                    ("404 Not Found", b"not found".to_vec())
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        Fake {
            original: fs::read(&exe).unwrap(),
            api: format!("http://127.0.0.1:{port}/release.json"),
            root,
            hits,
        }
    }

    fn exe(&self) -> PathBuf {
        self.root.join("bin/ultracompress")
    }

    fn previous(&self) -> PathBuf {
        self.root.join("bin/ultracompress.previous")
    }

    fn cache(&self) -> PathBuf {
        self.root.join("cache/update-check.json")
    }

    fn config(&self) -> PathBuf {
        self.root.join("config/config.json")
    }

    fn env(&self, command: &mut Command) {
        for key in [
            "CI",
            "CLAUDECODE",
            "ULTRACOMPRESS_NO_UPDATE_CHECK",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "CARGO_HOME",
        ] {
            command.env_remove(key);
        }
        command
            .env("HOME", self.root.join("home"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("ULTRACOMPRESS_UPDATE_API", &self.api)
            .env("ULTRACOMPRESS_CACHE_DIR", self.root.join("cache"))
            .env("ULTRACOMPRESS_CONFIG_DIR", self.root.join("config"));
    }

    /// Piped stdio, as in an agent hook or a script.
    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(self.exe());
        command.args(args);
        self.env(&mut command);
        command.output().unwrap()
    }

    /// stdout and stderr on a pseudo-terminal, as in an interactive shell.
    fn run_tty(&self, args: &str) -> String {
        let line = format!("{} {args}", self.exe().display());
        let mut command = Command::new("script");
        if cfg!(target_os = "macos") {
            command.args(["-q", "/dev/null", "sh", "-c", &line]);
        } else {
            command.args(["-qec", &line, "/dev/null"]);
        }
        self.env(&mut command);
        let output = command.output().expect("script(1) provides the terminal");
        // macOS script(1) echoes the end of its closed stdin as "^D\b\b".
        String::from_utf8_lossy(&output.stdout)
            .replace('\r', "")
            .replace("^D\u{8}\u{8}", "")
    }

    fn json(&self, path: &Path) -> serde_json::Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    fn temp_is_empty(&self) -> bool {
        fs::read_dir(self.root.join("tmp"))
            .unwrap()
            .next()
            .is_none()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn request_path(stream: &mut std::net::TcpStream) -> String {
    let mut request = Vec::new();
    let mut buf = [0u8; 1024];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => request.extend_from_slice(&buf[..n]),
        }
    }
    let request = String::from_utf8_lossy(&request);
    request.split_whitespace().nth(1).unwrap_or("/").to_string()
}

/// The release layout: macOS `cli/ultracompress` in a zip, Linux the binary in a tar.gz.
fn build_archive(root: &Path) -> Vec<u8> {
    let cli = root.join("stage/cli");
    let binary = cli.join("ultracompress");
    fs::write(&binary, NEW_BINARY).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    let archive = root.join("stage").join(asset());
    let status = if cfg!(target_os = "macos") {
        Command::new("ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(&cli)
            .arg(&archive)
            .status()
    } else {
        Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&cli)
            .arg("ultracompress")
            .status()
    };
    assert!(status.unwrap().success());
    fs::read(archive).unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn wait_for(what: &str, done: impl Fn() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn prime_cache(fake: &Fake, latest: &str) {
    fs::create_dir_all(fake.root.join("cache")).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let cache = serde_json::json!({ "checkedAt": now, "latest": latest });
    fs::write(fake.cache(), cache.to_string()).unwrap();
}

#[test]
fn check_exit_codes_and_failure_cause() {
    let fake = Fake::new("check", "v9.9.9", false, true);
    let out = fake.run(&["update", "--check"]);
    assert_eq!(out.status.code(), Some(10), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("ultracompress {NEW} available (you have {VERSION})\n")
    );
    assert_eq!(fake.json(&fake.cache())["latest"], NEW);
    assert_eq!(fake.hits.load(Ordering::SeqCst), 1);

    let pre = Fake::new("check-pre", "v9.9.9", true, true);
    let out = pre.run(&["update", "--check"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stdout(&out),
        format!("ultracompress {VERSION} is up to date.\n")
    );

    let offline = Fake::new("check-offline", "v9.9.9", false, true);
    let mut command = Command::new(offline.exe());
    offline.env(&mut command);
    let out = command
        .args(["update", "--check"])
        .env(
            "ULTRACOMPRESS_UPDATE_API",
            "http://127.0.0.1:1/release.json",
        )
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).starts_with("ultracompress: update check failed: http://127.0.0.1:1/"));
    let cause = offline.json(&offline.cache())["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        cause.starts_with("http://127.0.0.1:1/release.json: curl: (7)"),
        "{cause}"
    );
}

#[test]
fn install_verifies_swaps_keeps_one_previous_and_cleans_temp() {
    let fake = Fake::new("install", "v9.9.9", false, true);
    // Not a terminal: the explicit command is consent, no question asked.
    let out = fake.run(&["update"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.starts_with(&format!(
            "ultracompress {VERSION} → {NEW}\n  Fixes\n  - one\n"
        )),
        "{text}"
    );
    assert!(text.ends_with(&format!(
        "Installed ultracompress {NEW}. To go back: ultracompress update --rollback\n"
    )));
    assert_eq!(fs::read_to_string(fake.exe()).unwrap(), NEW_BINARY);
    assert_eq!(fs::read(fake.previous()).unwrap(), fake.original);
    assert!(fake.temp_is_empty(), "download folder left behind");
    let mut names: Vec<_> = fs::read_dir(fake.root.join("bin"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["ultracompress", "ultracompress.previous"]);
    assert_eq!(fake.json(&fake.cache())["installed"], NEW);
}

#[test]
fn checksum_mismatch_changes_nothing() {
    let fake = Fake::new("mismatch", "v9.9.9", false, false);
    let out = fake.run(&["update", "--yes"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        format!(
            "ultracompress: checksum mismatch for {}; nothing was installed\n",
            asset()
        )
    );
    assert_eq!(fs::read(fake.exe()).unwrap(), fake.original);
    assert!(!fake.previous().exists());
    assert!(fake.temp_is_empty(), "download folder left behind");
}

#[test]
fn rollback_restores_and_skips_the_version_rolled_back_from() {
    let fake = Fake::new("rollback", "v9.9.9", false, true);
    let out = fake.run(&["update", "--rollback"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("nothing to roll back"));

    fs::write(fake.previous(), "older build").unwrap();
    let out = fake.run(&["update", "--rollback"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(fs::read_to_string(fake.exe()).unwrap(), "older build");
    assert!(!fake.previous().exists());
    assert_eq!(fake.json(&fake.config())["skipVersion"], VERSION);
}

#[test]
fn previous_copy_goes_after_the_new_version_runs_once() {
    let fake = Fake::new("settle", "v9.9.9", false, true);
    fs::write(fake.previous(), "older build").unwrap();
    // Not yet marked as installed by an update: the copy stays.
    assert!(fake.run(&["version"]).status.success());
    assert!(fake.previous().exists());
    fs::create_dir_all(fake.root.join("cache")).unwrap();
    fs::write(fake.cache(), format!("{{\"installed\":\"{VERSION}\"}}")).unwrap();
    assert!(fake.run(&["version"]).status.success());
    assert!(!fake.previous().exists());
}

#[test]
fn skip_auto_and_clean_persist_and_remove_state() {
    let fake = Fake::new("choices", "v9.9.9", false, true);
    let out = fake.run(&["update", "--skip"]);
    assert_eq!(
        stdout(&out),
        format!("Skipped {NEW}. A newer release will notify again.\n")
    );
    assert_eq!(fake.json(&fake.config())["skipVersion"], NEW);
    assert_eq!(stdout(&fake.run(&["update", "--notice"])), "{}\n");
    let out = fake.run(&["update", "--check"]);
    assert_eq!(out.status.code(), Some(10));
    assert!(stdout(&out).contains("· skipped"));

    assert_eq!(
        stdout(&fake.run(&["update", "--auto", "status"])),
        "Automatic updates: off\n"
    );
    assert_eq!(
        stdout(&fake.run(&["update", "--auto", "on"])),
        "Automatic updates: on\n"
    );
    assert_eq!(fake.json(&fake.config())["installAutomatically"], true);
    fs::write(fake.root.join("cache/update.log"), "old log").unwrap();
    assert_eq!(
        stdout(&fake.run(&["update", "--auto", "off"])),
        "Automatic updates: off\n"
    );
    assert_eq!(fake.json(&fake.config())["installAutomatically"], false);
    assert!(!fake.root.join("cache/update.log").exists());
    assert_eq!(
        fake.run(&["update", "--auto", "maybe"]).status.code(),
        Some(2)
    );
    assert_eq!(fake.run(&["update", "--bogus"]).status.code(), Some(2));

    fs::write(fake.previous(), "older build").unwrap();
    assert!(fake.run(&["update", "--clean"]).status.success());
    assert!(!fake.cache().exists() && !fake.config().exists() && !fake.previous().exists());
}

#[test]
fn no_notice_or_check_without_a_terminal_or_when_opted_out() {
    let fake = Fake::new("quiet", "v9.9.9", false, true);
    // Stale cache: a terminal run would start a check; a piped run must not.
    let out = fake.run(&["version"]);
    assert_eq!(stdout(&out), format!("ultracompress {VERSION}\n"));
    assert!(out.stderr.is_empty());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(fake.hits.load(Ordering::SeqCst), 0);
    assert!(
        !fake.root.join("cache").exists(),
        "piped run wrote update state"
    );

    // A known newer release still prints nothing into piped output.
    prime_cache(&fake, NEW);
    let before = fs::read(fake.cache()).unwrap();
    let out = fake.run(&["version"]);
    assert!(out.stderr.is_empty());
    assert_eq!(fs::read(fake.cache()).unwrap(), before);

    // Host footer (`--notice`): once per version per day; silent with CI or the opt-out.
    for (key, value) in [("CI", "true"), ("ULTRACOMPRESS_NO_UPDATE_CHECK", "1")] {
        let mut command = Command::new(fake.exe());
        fake.env(&mut command);
        let out = command
            .args(["update", "--notice"])
            .env(key, value)
            .output()
            .unwrap();
        assert_eq!(stdout(&out), "{}\n", "{key}");
    }
    let first = stdout(&fake.run(&["update", "--notice"]));
    assert_eq!(
        first,
        format!("{{\"current\":\"{VERSION}\",\"latest\":\"{NEW}\"}}\n")
    );
    assert_eq!(stdout(&fake.run(&["update", "--notice"])), "{}\n");
    assert_eq!(fake.hits.load(Ordering::SeqCst), 0);
}

#[test]
fn terminal_run_checks_in_the_background_then_notifies_once() {
    let fake = Fake::new("tty", "v9.9.9", false, true);
    let first = fake.run_tty("version");
    assert_eq!(
        first,
        format!("ultracompress {VERSION}\n"),
        "no notice before a check"
    );
    wait_for("the background check", || {
        fs::read(fake.cache())
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .is_some_and(|c| c["latest"] == NEW)
    });
    let second = fake.run_tty("version");
    assert_eq!(
        second,
        format!(
            "ultracompress {VERSION}\n\
             ultracompress {NEW} available (you have {VERSION}) · run: ultracompress update\n\
             \x1b[2mturn on automatic updates: ultracompress update --auto on · skip: ultracompress update --skip\x1b[0m\n"
        )
    );
    assert_eq!(
        fake.run_tty("version"),
        format!("ultracompress {VERSION}\n"),
        "once a day"
    );
    assert_eq!(fake.hits.load(Ordering::SeqCst), 1, "one request per day");
}

#[test]
fn automatic_install_runs_in_the_background_when_opted_in() {
    let fake = Fake::new("auto", "v9.9.9", false, true);
    assert!(fake.run(&["update", "--auto", "on"]).status.success());
    let out = fake.run_tty("version");
    assert_eq!(out, format!("ultracompress {VERSION}\n"));
    wait_for("the background install", || {
        fs::read_to_string(fake.exe()).is_ok_and(|text| text == NEW_BINARY)
    });
    wait_for("the install log", || {
        fs::read_to_string(fake.root.join("cache/update.log"))
            .is_ok_and(|log| log.contains("installed 9.9.9"))
    });
    assert_eq!(fs::read(fake.previous()).unwrap(), fake.original);
    let log = fs::metadata(fake.root.join("cache/update.log"))
        .unwrap()
        .len();
    assert!(log < 256 * 1024);
    assert!(fake.temp_is_empty(), "download folder left behind");
    let cache = fake.json(&fake.cache());
    assert_eq!(
        (&cache["installed"], &cache["autoTried"]),
        (&NEW.into(), &NEW.into())
    );
}

#[test]
fn package_manager_installs_get_the_managers_command() {
    let fake = Fake::new("cargo", "v9.9.9", false, true);
    let cargo_bin = fake.root.join("home/.cargo/bin");
    fs::create_dir_all(&cargo_bin).unwrap();
    fs::copy(fake.exe(), cargo_bin.join("ultracompress")).unwrap();
    let mut command = Command::new(cargo_bin.join("ultracompress"));
    fake.env(&mut command);
    let out = command.args(["update", "--check"]).output().unwrap();
    assert_eq!(out.status.code(), Some(10));
    let text = stdout(&out);
    assert!(
        text.contains(
            "Installed with Cargo; update with: cargo install --locked --git https://github.com/"
        ) && text.contains("--tag v9.9.9 ultracompress"),
        "{text}"
    );
}
