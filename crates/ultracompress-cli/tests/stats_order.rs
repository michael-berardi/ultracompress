//! `stats` output must be byte-stable across runs: its per-role breakdown
//! used to follow HashMap iteration order, which differs per process.

use std::process::Command;

#[test]
fn stats_by_role_is_sorted_and_stable() {
    let dir = std::env::temp_dir().join(format!("uc-stats-order-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let session = dir.join("session.jsonl");
    let lines = [
        r#"{"type":"session","id":"s1","cwd":"/tmp/p"}"#,
        r#"{"type":"message","id":"e1","parentId":null,"message":{"role":"user","content":[{"type":"text","text":"fix it"}]}}"#,
        r#"{"type":"message","id":"e2","parentId":"e1","message":{"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"bash","arguments":{"command":"make"}}]}}"#,
        r#"{"type":"message","id":"e3","parentId":"e2","message":{"role":"toolResult","toolCallId":"c1","toolName":"bash","content":[{"type":"text","text":"ok"}]}}"#,
        r#"{"type":"message","id":"e4","parentId":"e3","message":{"role":"system","content":[{"type":"text","text":"note"}]}}"#,
    ];
    std::fs::write(&session, lines.join("\n")).unwrap();

    let mut outputs = Vec::new();
    for _ in 0..10 {
        let out = Command::new(env!("CARGO_BIN_EXE_ultracompress"))
            .args(["stats", "--session"])
            .arg(&session)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        outputs.push(String::from_utf8(out.stdout).unwrap());
    }
    std::fs::remove_dir_all(&dir).ok();

    assert!(
        outputs.windows(2).all(|w| w[0] == w[1]),
        "stats output varied between runs"
    );
    let parsed: serde_json::Value = serde_json::from_str(&outputs[0]).unwrap();
    let roles: Vec<&str> = parsed["byRole"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["role"].as_str().unwrap())
        .collect();
    let mut sorted = roles.clone();
    sorted.sort();
    assert_eq!(roles, sorted);
    assert_eq!(roles.len(), 4);
}
