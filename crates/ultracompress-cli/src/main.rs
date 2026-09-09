//! `ultracompress` — the UltraCompress CLI.
//!
//! Subcommands:
//!   ultracompress plan        dry-run: policy decisions + token estimates, no frames
//!   ultracompress compact     full compaction result (summary, details, stats)
//!   ultracompress transform   live-context ops: UC packets + snap frames per block
//!   ultracompress recall      ranked lossless search over a raw session JSONL
//!   ultracompress stats       per-role content breakdown of a session
//!   ultracompress frames      render text to PNG frames (debugging / standalone use)
//!   ultracompress uc          UC packet encode/decode bridge (degrades to plain JSON)
//!   ultracompress version     print version
//!
//! Input: `--session FILE` (Pi session JSONL) or stdin JSON `{ "entries": [...] }`
//! (the extension contract). Output: JSON on stdout, diagnostics on stderr.

use serde_json::{json, Value};
use std::path::PathBuf;
use ultracompress_core::compact::{run, CompactInput};
use ultracompress_core::load::{load_session, read_stdin};
use ultracompress_core::policy::{Policy, VisionMode};
use ultracompress_core::recall::{search, RecallOptions};
use ultracompress_core::snap::{render_frames, SnapConfig};

fn die(msg: &str) -> ! {
    eprintln!("ultracompress: {msg}");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or_else(|| {
        die("usage: ultracompress <plan|compact|recall|stats|frames|version> [options]")
    });

    match cmd {
        "version" | "--version" | "-V" => {
            println!("ultracompress {}", ultracompress_core::VERSION);
        }
        "plan" => main_plan(&args[1..], true),
        "compact" => main_plan(&args[1..], false),
        "transform" => main_transform(&args[1..]),
        "recall" => main_recall(&args[1..]),
        "stats" => main_stats(&args[1..]),
        "uc" => main_uc(&args[1..]),
        "frames" => main_frames(&args[1..]),
        other => die(&format!("unknown command '{other}'")),
    }
}

struct Cli {
    session: Option<PathBuf>,
    policy: Policy,
    keep: Option<usize>,
    smart: bool,
    vision: VisionMode,
    uc_bin: String,
    uc_enabled: bool,
    snap_min: usize,
    uc_min: usize,
    cols: usize,
    rows: usize,
    query: Option<String>,
    regex: bool,
    scope_all: bool,
    page: usize,
    per_page: usize,
    image_tokens: Option<u64>,
    label: String,
    recall: RecallOptions,
}

fn parse_cli(args: &[String]) -> Cli {
    let mut c = Cli {
        session: None,
        policy: Policy::Auto,
        keep: None,
        smart: true,
        vision: VisionMode::Auto,
        uc_bin: "uc".into(),
        uc_enabled: true,
        snap_min: 6000,
        uc_min: 1200,
        cols: 160,
        rows: 100,
        query: None,
        regex: false,
        scope_all: false,
        page: 1,
        per_page: 5,
        image_tokens: None,
        label: "text".into(),
        recall: RecallOptions::default(),
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let val = |i: &mut usize| -> String {
            *i += 1;
            args.get(*i)
                .cloned()
                .unwrap_or_else(|| die("missing option value"))
        };
        match a {
            "--session" => c.session = Some(PathBuf::from(val(&mut i))),
            "--policy" => {
                c.policy = Policy::parse(&val(&mut i))
                    .unwrap_or_else(|| die("bad --policy (auto|vcc|snap|uc)"))
            }
            "--keep-user-turns" => {
                c.keep = Some(
                    val(&mut i)
                        .parse()
                        .unwrap_or_else(|_| die("bad keep value")),
                )
            }
            "--keep" => {
                c.keep = Some(
                    val(&mut i)
                        .parse()
                        .unwrap_or_else(|_| die("bad keep value")),
                )
            }
            "--keep-default" => c.keep = None,
            "--no-smart-keep" => c.smart = false,
            "--vision" => {
                c.vision = match val(&mut i).to_lowercase().as_str() {
                    "on" => VisionMode::On,
                    "off" => VisionMode::Off,
                    _ => VisionMode::Auto,
                }
            }
            "--uc-bin" => c.uc_bin = val(&mut i),
            "--no-uc" => c.uc_enabled = false,
            "--uc-min-chars" => {
                c.uc_min = val(&mut i).parse().unwrap_or_else(|_| die("bad number"))
            }
            "--snap-min-chars" => {
                c.snap_min = val(&mut i).parse().unwrap_or_else(|_| die("bad number"))
            }
            "--cols" => c.cols = val(&mut i).parse().unwrap_or_else(|_| die("bad number")),
            "--rows" => c.rows = val(&mut i).parse().unwrap_or_else(|_| die("bad number")),
            "--image-tokens-per-frame" => {
                c.image_tokens = Some(val(&mut i).parse().unwrap_or_else(|_| die("bad number")))
            }
            "--query" => c.query = Some(val(&mut i)),
            "--regex" => c.regex = true,
            "--scope" => {
                c.scope_all = match val(&mut i).as_str() {
                    "all" => true,
                    "lineage" => false,
                    _ => die("bad --scope (lineage|all; both are session-local)"),
                }
            }
            "--leaf" => c.recall.leaf_id = Some(val(&mut i)),
            "--role" => c.recall.role = Some(val(&mut i)),
            "--tool-name" => c.recall.tool_name = Some(val(&mut i)),
            "--after-entry" => c.recall.after_entry = Some(val(&mut i)),
            "--before-entry" => c.recall.before_entry = Some(val(&mut i)),
            "--snippet-bytes" => {
                c.recall.snippet_bytes = val(&mut i).parse().unwrap_or_else(|_| die("bad number"))
            }
            "--max-output-bytes" => {
                c.recall.max_output_bytes =
                    val(&mut i).parse().unwrap_or_else(|_| die("bad number"))
            }
            "--page" => c.page = val(&mut i).parse().unwrap_or_else(|_| die("bad number")),
            "--per-page" => c.per_page = val(&mut i).parse().unwrap_or_else(|_| die("bad number")),
            "--label" => c.label = val(&mut i),
            other => die(&format!("unknown option '{other}'")),
        }
        i += 1;
    }
    c
}

/// Load entries either from --session or stdin `{ "entries": [...] }`.
fn load_entries(cli: &Cli) -> (Vec<serde_json::Value>, Option<u64>, Value) {
    if let Some(path) = &cli.session {
        let raw = std::fs::read_to_string(path)
            .unwrap_or_else(|e| die(&format!("cannot load session: {e}")));
        let entries = raw
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
            .filter(|v| {
                matches!(
                    v.get("type").and_then(|t| t.as_str()),
                    Some("message") | Some("compaction")
                )
            })
            .collect();
        return (entries, None, Value::Null);
    }
    let bytes = read_stdin().unwrap_or_else(|e| die(&e.to_string()));
    let mut v: serde_json::Value =
        serde_json::from_slice(&bytes).unwrap_or_else(|e| die(&format!("bad stdin JSON: {e}")));
    // Move large entry arrays rather than cloning them and retaining a second
    // full session inside the settings envelope throughout compaction.
    if let Some(entries) = v.as_object_mut().and_then(|obj| obj.remove("entries")) {
        let tb = v.get("tokensBefore").and_then(|t| t.as_u64());
        let Value::Array(entries) = entries else {
            die("entries must be an array")
        };
        (entries, tb, v)
    } else if let Value::Array(entries) = v {
        (entries, None, Value::Null)
    } else {
        die("stdin JSON must be { \"entries\": [...] } or a message array")
    }
}

fn previous_summary(cli: &Cli) -> Option<String> {
    if let Some(path) = &cli.session {
        let raw = std::fs::read_to_string(path).ok()?;
        let last = raw
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
            .rfind(|v| v.get("type").and_then(|t| t.as_str()) == Some("compaction"))?;
        return last
            .get("summary")
            .and_then(|s| s.as_str())
            .map(String::from);
    }
    None
}

/// Adapter JSON settings are defaults; explicit CLI options take precedence.
/// Decode types strictly so a malformed setting does not silently change policy.
fn stdin_setting<T: serde::de::DeserializeOwned>(v: &Value, key: &str) -> Option<T> {
    v.get(key).filter(|value| !value.is_null()).map(|value| {
        serde_json::from_value(value.clone()).unwrap_or_else(|e| die(&format!("bad {key}: {e}")))
    })
}

fn apply_stdin_settings(cli: &mut Cli, v: &Value, args: &[String]) {
    let has = |flags: &[&str]| args.iter().any(|arg| flags.contains(&arg.as_str()));
    if !has(&["--policy"]) {
        if let Some(value) = stdin_setting(v, "policy") {
            cli.policy = value;
        }
    }
    if !has(&["--vision"]) {
        if let Some(value) = stdin_setting(v, "vision") {
            cli.vision = value;
        }
    }
    if !has(&["--keep", "--keep-user-turns", "--keep-default"]) && v.get("keepUserTurns").is_some()
    {
        cli.keep = stdin_setting(v, "keepUserTurns");
    }
    if !has(&["--no-smart-keep"]) {
        if let Some(value) = stdin_setting(v, "smartKeepTail") {
            cli.smart = value;
        }
    }
    if !has(&["--uc-bin"]) {
        if let Some(value) = stdin_setting(v, "ucBin") {
            cli.uc_bin = value;
        }
    }
    if !has(&["--no-uc"]) {
        if let Some(value) = stdin_setting(v, "ucEnabled") {
            cli.uc_enabled = value;
        }
    }
    if !has(&["--uc-min-chars"]) {
        if let Some(value) = stdin_setting(v, "ucMinChars") {
            cli.uc_min = value;
        }
    }
    if !has(&["--snap-min-chars"]) {
        if let Some(value) = stdin_setting(v, "snapMinChars") {
            cli.snap_min = value;
        }
    }
    if !has(&["--image-tokens-per-frame"]) {
        if let Some(value) = stdin_setting(v, "imageTokensPerFrame") {
            cli.image_tokens = Some(value);
        }
    }
}

fn main_plan(args: &[String], dry: bool) {
    let mut cli = parse_cli(args);
    let (entries, tokens_before, settings) = load_entries(&cli);
    apply_stdin_settings(&mut cli, &settings, args);
    let input = CompactInput {
        entries,
        tokens_before,
        previous_summary: stdin_setting(&settings, "previousSummary")
            .or_else(|| previous_summary(&cli)),
        policy: cli.policy,
        keep_user_turns: cli.keep,
        smart_keep_tail: cli.smart,
        vision: cli.vision,
        model_vision: stdin_setting(&settings, "modelVision"),
        uc_bin: Some(cli.uc_bin.clone()),
        uc_enabled: cli.uc_enabled,
        thresholds: Some(ultracompress_core::classify::Thresholds {
            uc_min_chars: cli.uc_min,
            snap_min_chars: cli.snap_min,
        }),
        snap: Some(SnapConfig {
            cols: cli.cols,
            rows: cli.rows,
            ..Default::default()
        }),
        transcript: None,
        image_tokens_per_frame: cli.image_tokens,
        dry_run: dry,
    };
    let result = run(&input).unwrap_or_else(|e| die(&e));
    let mut out = serde_json::to_value(&result).unwrap_or_else(|e| die(&e.to_string()));
    if dry {
        if let Some(obj) = out.as_object_mut() {
            obj.remove("summary");
        }
    }
    println!(
        "{}",
        serde_json::to_string(&out).unwrap_or_else(|e| die(&e.to_string()))
    );
}

fn main_transform(args: &[String]) {
    let mut cli = parse_cli(args);
    let bytes = read_stdin().unwrap_or_else(|e| die(&e.to_string()));
    let mut v: serde_json::Value =
        serde_json::from_slice(&bytes).unwrap_or_else(|e| die(&format!("bad stdin JSON: {e}")));
    apply_stdin_settings(&mut cli, &v, args);
    let messages = if v.is_array() {
        std::mem::take(&mut v)
    } else {
        v.as_object_mut()
            .and_then(|obj| obj.remove("messages"))
            .unwrap_or_else(|| die("stdin must be a message array or { \"messages\": [...] }"))
    };
    let Value::Array(messages) = messages else {
        die("messages must be an array")
    };
    let input = ultracompress_core::transform::TransformInput {
        messages,
        policy: cli.policy,
        vision: cli.vision,
        model_vision: v.get("modelVision").and_then(|m| m.as_bool()),
        uc_bin: Some(cli.uc_bin.clone()),
        uc_enabled: cli.uc_enabled,
        thresholds: Some(ultracompress_core::classify::Thresholds {
            uc_min_chars: cli.uc_min,
            snap_min_chars: cli.snap_min,
        }),
        snap: Some(SnapConfig::default()),
        image_tokens_per_frame: cli.image_tokens,
        chars_per_token: v.get("charsPerToken").and_then(|c| c.as_f64()),
    };
    let result = ultracompress_core::transform::run(&input).unwrap_or_else(|e| die(&e));
    println!(
        "{}",
        serde_json::to_string(&result).unwrap_or_else(|e| die(&e.to_string()))
    );
}

fn main_recall(args: &[String]) {
    let cli = parse_cli(args);
    let path = cli
        .session
        .clone()
        .unwrap_or_else(|| die("recall requires --session FILE"));
    let query = cli
        .query
        .clone()
        .unwrap_or_else(|| die("recall requires --query"));
    let opts = RecallOptions {
        query,
        regex: cli.regex,
        scope_all: cli.scope_all,
        page: cli.page,
        per_page: cli.per_page,
        ..cli.recall
    };
    let result = search(&path, &opts).unwrap_or_else(|e| die(&e));
    println!(
        "{}",
        serde_json::to_string(&result).unwrap_or_else(|e| die(&e.to_string()))
    );
}

fn main_uc(args: &[String]) {
    let mode = args.first().map(|s| s.as_str()).unwrap_or("decode");
    let bytes = read_stdin().unwrap_or_else(|e| die(&e.to_string()));
    // Accept either { "packet": "..." } JSON or raw packet text on stdin.
    let input = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
    let payload = input
        .as_ref()
        .and_then(|v| v.get("packet"))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| String::from_utf8_lossy(&bytes).to_string());
    // Match the encoder's configured binary instead of silently using another PATH entry.
    let uc_bin = input
        .as_ref()
        .and_then(|v| v.get("ucBin"))
        .and_then(|v| v.as_str())
        .filter(|bin| !bin.is_empty())
        .unwrap_or("uc");
    let sub = match mode {
        "encode" | "decode" => mode,
        other => die(&format!("unknown uc mode '{other}' (encode|decode)")),
    };
    use std::io::Write;
    let mut child = std::process::Command::new(uc_bin)
        .arg(sub)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| die(&format!("cannot spawn uc: {e}")));
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap_or_else(|e| die(&e.to_string()));
    let out = child
        .wait_with_output()
        .unwrap_or_else(|e| die(&e.to_string()));
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let hint = if sub == "decode" {
            " Use the uc:<hash> reference if available, or recover the original with ultracompress_recall. Do not abbreviate or reconstruct a packet, and do not retry the same invalid text."
        } else {
            ""
        };
        println!("{}", json!({ "error": format!("{}{hint}", err.trim()) }));
        return;
    }
    println!(
        "{}",
        json!({ "decoded": String::from_utf8_lossy(&out.stdout) })
    );
}

fn main_stats(args: &[String]) {
    let cli = parse_cli(args);
    let path = cli
        .session
        .clone()
        .unwrap_or_else(|| die("stats requires --session FILE"));
    let s = load_session(&path, true).unwrap_or_else(|e| die(&format!("cannot load session: {e}")));
    let mut by_role: std::collections::HashMap<String, (usize, u64)> =
        std::collections::HashMap::new();
    let mut json_blocks = 0usize;
    for m in &s.messages {
        let e = by_role.entry(m.role.to_string()).or_default();
        e.0 += 1;
        e.1 += m.total_chars() as u64;
        for b in &m.content {
            if let ultracompress_core::model::Block::ToolResult { text, .. } = b {
                if ultracompress_core::classify::classify_content(text)
                    == ultracompress_core::classify::ContentClass::Json
                {
                    json_blocks += 1;
                }
            }
        }
    }
    let total_chars: u64 = by_role.values().map(|(_, c)| c).sum();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "session": s.session_id,
            "cwd": s.cwd,
            "entries": s.entry_count,
            "messages": s.messages.len(),
            "totalChars": total_chars,
            "estTokens": total_chars as f64 / 3.8,
            "jsonToolResults": json_blocks,
            "byRole": by_role.iter().map(|(k, (n, c))| json!({"role": k, "messages": n, "chars": c})).collect::<Vec<_>>(),
        }))
        .unwrap_or_else(|e| die(&e.to_string()))
    );
}

fn main_frames(args: &[String]) {
    let cli = parse_cli(args);
    let mut text = String::new();
    let has_session = cli.session.is_some();
    if !has_session {
        use std::io::Read;
        std::io::stdin()
            .read_to_string(&mut text)
            .unwrap_or_else(|e| die(&e.to_string()));
    } else {
        die("frames reads text on stdin");
    }
    let cfg = SnapConfig {
        cols: cli.cols,
        rows: cli.rows,
        ..Default::default()
    };
    let r = render_frames(&text, &cli.label, &cfg);
    println!(
        "{}",
        serde_json::to_string(&json!({
            "frames": r.frames,
            "head": r.head,
            "tail": r.tail,
            "sourceChars": r.source_chars,
            "archivedChars": r.archived_chars,
        }))
        .unwrap_or_else(|e| die(&e.to_string()))
    );
}
