//! Claude Code transcript recall: format auto-detection, tool mapping,
//! compaction-spanning lineage, sidechain/meta skipping, malformed-line
//! tolerance, and Pi regression.

use serde_json::json;
use std::path::PathBuf;
use ultracompress_core::recall::{search, RecallFormat, RecallOptions};

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("claude")
        .join(name)
}

fn opts(query: &str) -> RecallOptions {
    RecallOptions {
        query: query.into(),
        ..Default::default()
    }
}

fn ids(r: &ultracompress_core::recall::RecallResult) -> Vec<String> {
    r.hits.iter().map(|h| h.entry_id.clone()).collect()
}

/// Build a classic Pi session file (session header + id/parentId entries).
fn pi_fixture(id: &str, records: Vec<serde_json::Value>) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("uc-claude-pi-{}-{}.jsonl", std::process::id(), id));
    let mut lines =
        vec![json!({"type":"session","version":3,"id":id,"cwd":"/synthetic"}).to_string()];
    lines.extend(records.into_iter().map(|r| r.to_string()));
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    path
}

#[test]
fn auto_detects_claude_transcripts_and_maps_messages() {
    let f = data("plain-chat.jsonl");
    let r = search(&f, &opts("ZEBRA-QUANTUM")).unwrap();
    assert_eq!(r.scope, "lineage");
    // lineage from the newest record u2 walks parents back through the
    // auxiliary chain nodes to u1 and a1
    assert_eq!(r.leaf_id.as_deref(), Some("u2"));
    assert_eq!(r.searched_messages, 4);
    assert_eq!(r.total, 2);
    assert!(ids(&r).contains(&"u1".to_string()));
    assert!(ids(&r).contains(&"a1".to_string()));
    assert_eq!(r.warnings, 0);
    // ISO 8601 record timestamps map to epoch seconds on hits
    let u1 = r.hits.iter().find(|h| h.entry_id == "u1").unwrap();
    assert_eq!(u1.timestamp, Some(1767323045));
    assert_eq!(u1.role, "user");
    // role filter narrows before ranking
    let mut o = opts("ZEBRA-QUANTUM");
    o.role = Some("user".into());
    let r = search(&f, &o).unwrap();
    assert_eq!(ids(&r), ["u1"]);
    // informational system records are searchable prose with role system…
    let r = search(&f, &opts("OYSTER-MODE")).unwrap();
    assert_eq!(r.total, 1);
    assert_eq!(r.hits[0].role, "system");
    // …while content-free auxiliary records stay pure chain nodes
    assert_eq!(search(&f, &opts("turn_duration")).unwrap().total, 0);
}

#[test]
fn tool_results_resolve_names_from_earlier_tool_uses() {
    let f = data("tool-pair.jsonl");
    let r = search(&f, &opts("HONEYDEW-RESULT")).unwrap();
    assert_eq!(ids(&r), ["u2", "a2"]);
    assert_eq!(r.hits[0].role, "toolResult");
    assert!(r.hits[0].snippet.contains("[Read]"));
    // tool-name filter hits the resolved result but not the assistant call
    let mut o = opts("HONEYDEW-RESULT");
    o.tool_name = Some("Read".into());
    assert_eq!(ids(&search(&f, &o).unwrap()), ["u2"]);
    o.role = Some("assistant".into());
    assert_eq!(search(&f, &o).unwrap().total, 0);
    // tool_use arguments are searchable through the call line
    let r = search(&f, &opts("notes.txt")).unwrap();
    assert_eq!(ids(&r), ["a1"]);
    // a result whose tool_use id never appeared keeps an empty tool name
    let mut o = opts("orphan remainder");
    o.tool_name = Some("Read".into());
    assert_eq!(search(&f, &o).unwrap().total, 0);
    let r = search(&f, &opts("orphan remainder")).unwrap();
    assert_eq!(ids(&r), ["u3"]);
}

#[test]
fn lineage_spans_compaction_via_logical_parent_and_keeps_pre_history() {
    let f = data("compaction.jsonl");
    // default lineage reaches across the compact boundary into the records
    // the live context lost
    let r = search(&f, &opts("APPLE-ORCHARD")).unwrap();
    assert_eq!(r.leaf_id.as_deref(), Some("a2"));
    assert_eq!(r.total, 2);
    assert!(ids(&r).contains(&"u1".to_string()));
    // explicit uuid leaf narrows the lineage
    let mut o = opts("APPLE-ORCHARD");
    o.leaf_id = Some("u1".into());
    let r = search(&f, &o).unwrap();
    assert_eq!(ids(&r), ["u1"]);
    o.leaf_id = Some("b1".into());
    assert_eq!(search(&f, &o).unwrap().total, 2);
    o.leaf_id = Some(String::new());
    assert_eq!(search(&f, &o).unwrap().searched_messages, 0);
    // the compact summary itself is a searchable user record
    let r = search(&f, &opts("PEAR-CELLAR")).unwrap();
    assert_eq!(ids(&r), ["u2", "a2"]);
    // all scope sees the same single session tree
    let mut o = opts("APPLE-ORCHARD");
    o.scope_all = true;
    o.leaf_id = None;
    let r = search(&f, &o).unwrap();
    assert_eq!(r.scope, "all");
    assert_eq!(r.total, 2);
}

#[test]
fn sidechains_never_hit_and_default_leaf_stays_on_the_main_track() {
    let f = data("sidechain.jsonl");
    // default tip is u2b, a sibling branch: lineage is u1 -> u2b only
    let r = search(&f, &opts("MAIN-TRACK")).unwrap();
    assert_eq!(r.leaf_id.as_deref(), Some("u2b"));
    assert_eq!(ids(&r), ["u1"]);
    assert_eq!(search(&f, &opts("KESTREL-9")).unwrap().total, 0);
    // all scope widens to sibling branches but never into sidechains
    let mut o = opts("KESTREL-9");
    o.scope_all = true;
    o.leaf_id = None;
    assert_eq!(ids(&search(&f, &o).unwrap()), ["a2"]);
    o.query = "QUARTZ-FALCON".into();
    assert_eq!(search(&f, &o).unwrap().total, 0);
    assert_eq!(search(&f, &opts("QUARTZ-FALCON")).unwrap().total, 0);
    // an explicit uuid leaf selects its branch
    let mut o = opts("KESTREL-9");
    o.leaf_id = Some("a2".into());
    assert_eq!(ids(&search(&f, &o).unwrap()), ["a2"]);
}

#[test]
fn malformed_lines_warn_and_are_counted_not_fatal() {
    let f = data("malformed.jsonl");
    let r = search(&f, &opts("OBOE-SONATA")).unwrap();
    assert_eq!(r.total, 2);
    assert_eq!(r.warnings, 1);
    // unparseable free text still fails validation of the query, not parse
    assert!(search(&f, &opts(" ")).is_err());
}

#[test]
fn duplicate_uuids_keep_the_first_record() {
    let f = data("duplicate.jsonl");
    assert_eq!(search(&f, &opts("CHERRY-DELTA")).unwrap().total, 1);
    assert_eq!(search(&f, &opts("MANGO-TIDE")).unwrap().total, 0);
    let r = search(&f, &opts("duplicate")).unwrap();
    assert_eq!(r.warnings, 0);
    assert_eq!(r.leaf_id.as_deref(), Some("a1"));
}

#[test]
fn explicit_format_overrides_and_rejects_mismatched_files() {
    let claude = data("plain-chat.jsonl");
    let mut o = opts("ZEBRA-QUANTUM");
    o.format = RecallFormat::Pi;
    assert!(
        search(&claude, &o).is_err(),
        "forced Pi must not read Claude"
    );
    o.format = RecallFormat::Claude;
    let forced = search(&claude, &o).unwrap();
    o.format = RecallFormat::Auto;
    assert_eq!(search(&claude, &o).unwrap().total, forced.total);

    let pi = pi_fixture(
        "pi-regression",
        vec![
            json!({"type":"message","id":"e1","parentId":null,"message":{"role":"user","content":[{"type":"text","text":"ZEBRA-QUANTUM pi side"}]}}),
        ],
    );
    let mut o = opts("ZEBRA-QUANTUM");
    o.format = RecallFormat::Auto;
    let r = search(&pi, &o).unwrap();
    assert_eq!(r.session_id, "pi-regression");
    assert_eq!(ids(&r), ["e1"]);
    assert_eq!(r.warnings, 0);
    o.format = RecallFormat::Claude;
    assert!(search(&pi, &o).is_err(), "forced Claude must not read Pi");
    o.format = RecallFormat::Pi;
    assert_eq!(search(&pi, &o).unwrap().total, 1);
    let _ = std::fs::remove_file(&pi);
}
