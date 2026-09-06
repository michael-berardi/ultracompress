use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use ultracompress_core::recall::{search, RecallOptions};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new(id: &str, records: Vec<serde_json::Value>) -> Self {
        let path = std::env::temp_dir().join(format!(
            "uc-recall-{}-{}.jsonl",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut lines =
            vec![json!({"type":"session","version":3,"id":id,"cwd":"/synthetic"}).to_string()];
        lines.extend(records.into_iter().map(|r| r.to_string()));
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn msg(id: &str, parent: Option<&str>, role: &str, text: &str) -> serde_json::Value {
    json!({"type":"message","id":id,"parentId":parent,"message":{"role":role,"toolName":"read","content":[{"type":"text","text":text}]}})
}
fn opts() -> RecallOptions {
    RecallOptions {
        query: "needle".into(),
        ..Default::default()
    }
}
fn branch() -> Fixture {
    Fixture::new(
        "session-a",
        vec![
            msg("root", None, "user", "needle root"),
            msg("left", Some("root"), "assistant", "needle LEFT_ONLY"),
            json!({"type":"compaction","id":"compact","parentId":"left","summary":"summary","firstKeptEntryId":"tail"}),
            msg(
                "tail",
                Some("compact"),
                "toolResult",
                "needle compacted branch",
            ),
            msg("right", Some("root"), "assistant", "needle RIGHT_ONLY"),
            msg("other-root", None, "user", "needle DIFFERENT_ROOT"),
        ],
    )
}
#[test]
fn exact_current_tip_and_explicit_all_never_search_other_session() {
    let a = branch();
    let _b = Fixture::new(
        "session-b",
        vec![msg("secret", None, "user", "needle OTHER_SESSION_ONLY")],
    );
    let mut o = opts();
    o.leaf_id = Some("tail".into());
    let r = search(&a.0, &o).unwrap();
    assert_eq!(r.session_id, "session-a");
    assert_eq!(r.scope, "lineage");
    assert_eq!(r.leaf_id.as_deref(), Some("tail"));
    assert_eq!(r.total, 3);
    assert_eq!(r.searched_messages, 3);
    assert!(r.hits.iter().any(|h| h.entry_id == "left"));
    assert!(r
        .hits
        .iter()
        .all(|h| !["right", "other-root", "secret"].contains(&h.entry_id.as_str())));
    o.scope_all = true;
    o.leaf_id = None;
    let r = search(&a.0, &o).unwrap();
    assert_eq!(r.total, 5);
    assert_eq!(r.scope, "all");
    assert_eq!(r.leaf_id, None);
    assert!(r.hits.iter().all(|h| h.entry_id != "secret"));
}
#[test]
fn null_parent_stops_and_empty_active_tip_never_falls_back() {
    let a = branch();
    let r = search(&a.0, &opts()).unwrap();
    assert_eq!(r.total, 1);
    assert_eq!(r.hits[0].entry_id, "other-root");
    let mut o = opts();
    o.leaf_id = Some(String::new());
    assert_eq!(search(&a.0, &o).unwrap().searched_messages, 0);
    o.leaf_id = Some("absent".into());
    assert!(search(&a.0, &o).is_err());
    let empty = Fixture::new("empty", vec![]);
    assert!(search(&empty.0, &o).is_err());
}
#[test]
fn filters_and_exclusive_ranges_apply_before_ranking_counts() {
    let a = branch();
    let mut o = opts();
    o.leaf_id = Some("tail".into());
    o.role = Some("toolResult".into());
    o.tool_name = Some("read".into());
    let r = search(&a.0, &o).unwrap();
    assert_eq!(r.total, 1);
    assert_eq!(r.searched_messages, 1);
    assert_eq!(r.hits[0].entry_id, "tail");
    o.tool_name = Some("bash".into());
    assert_eq!(search(&a.0, &o).unwrap().total, 0);
    o.tool_name = None;
    o.role = None;
    o.after_entry = Some("root".into());
    o.before_entry = Some("compact".into());
    assert_eq!(search(&a.0, &o).unwrap().hits[0].entry_id, "left");
    o.before_entry = Some("right".into());
    assert!(search(&a.0, &o).is_err());
    o.before_entry = Some("root".into());
    assert!(search(&a.0, &o).is_err());
}
#[test]
fn invalid_bounds_and_selectors_fail_closed() {
    let a = branch();
    for o in [
        RecallOptions {
            per_page: 0,
            ..opts()
        },
        RecallOptions {
            per_page: 21,
            ..opts()
        },
        RecallOptions { page: 0, ..opts() },
        RecallOptions {
            page: usize::MAX,
            ..opts()
        },
        RecallOptions {
            snippet_bytes: 127,
            ..opts()
        },
        RecallOptions {
            max_output_bytes: 1023,
            ..opts()
        },
        RecallOptions {
            role: Some("invalid".into()),
            ..opts()
        },
        RecallOptions {
            query: " ".into(),
            ..opts()
        },
        RecallOptions {
            regex: true,
            query: "[".into(),
            ..opts()
        },
        RecallOptions {
            scope_all: true,
            leaf_id: Some(String::new()),
            ..opts()
        },
    ] {
        assert!(search(&a.0, &o).is_err());
    }
}
#[test]
fn unicode_case_expansion_excerpts_and_complete_json_are_byte_bounded() {
    let text = format!(
        "{}needle{}",
        "İ\n日本".repeat(100),
        "漢字\\\"\n".repeat(200)
    );
    let a = Fixture::new(
        "unicode",
        (0..12)
            .map(|i| msg(&format!("e{i}"), None, "user", &text))
            .collect(),
    );
    let o = RecallOptions {
        scope_all: true,
        per_page: 5,
        snippet_bytes: 128,
        max_output_bytes: 2048,
        ..opts()
    };
    let r = search(&a.0, &o).unwrap();
    assert_eq!(r.hits.len(), 5);
    assert_eq!(r.total, 12);
    assert_eq!(r.page_count, 3);
    for h in &r.hits {
        assert!(h.snippet.len() <= 128);
        assert!(h.snippet.contains("needle"));
    }
    assert!(serde_json::to_vec(&r).unwrap().len() <= 2048);
    let next = search(
        &a.0,
        &RecallOptions {
            page: 2,
            ..o.clone()
        },
    )
    .unwrap();
    assert!(next
        .hits
        .iter()
        .all(|h| r.hits.iter().all(|prev| prev.entry_id != h.entry_id)));
    let tiny = search(
        &a.0,
        &RecallOptions {
            max_output_bytes: 1024,
            ..o
        },
    )
    .unwrap();
    assert_eq!(tiny.hits.len(), 5);
    assert!(serde_json::to_vec(&tiny).unwrap().len() <= 1024);
}
#[test]
fn malformed_trees_reject_instead_of_chaining_to_siblings() {
    for records in [
        vec![msg("x", Some("missing"), "user", "needle")],
        vec![
            msg("x", Some("y"), "user", "needle"),
            msg("y", Some("x"), "user", "needle"),
        ],
        vec![
            msg("x", None, "user", "needle"),
            msg("x", None, "user", "needle"),
        ],
    ] {
        let f = Fixture::new("bad", records);
        assert!(search(&f.0, &opts()).is_err());
    }
    let f = Fixture::new("broken", vec![]);
    std::fs::write(&f.0, "garbage\n").unwrap();
    assert!(search(&f.0, &opts()).is_err());
}
#[test]
fn all_scope_rejects_malformed_records_and_parent_trees() {
    for records in [
        vec![msg("x", Some("missing"), "user", "needle")],
        vec![
            msg("x", Some("y"), "user", "needle"),
            msg("y", Some("x"), "user", "needle"),
        ],
        vec![json!({"id":"x","parentId":null})],
        vec![json!({"type":null,"id":"x","parentId":null})],
    ] {
        let file = Fixture::new("invalid-all", records);
        assert!(search(
            &file.0,
            &RecallOptions {
                scope_all: true,
                ..opts()
            }
        )
        .is_err());
    }
}
#[test]
fn tool_filter_excludes_assistant_calls_and_out_of_range_pages_error() {
    let file = Fixture::new(
        "tools",
        vec![
            json!({"type":"message","id":"call","parentId":null,"message":{"role":"assistant","content":[{"type":"text","text":"needle unrelated prose"},{"type":"toolCall","id":"t","name":"read","arguments":{"path":"needle"}}]}}),
            msg("result", Some("call"), "toolResult", "needle actual result"),
        ],
    );
    let result = search(
        &file.0,
        &RecallOptions {
            tool_name: Some("read".into()),
            ..opts()
        },
    )
    .unwrap();
    assert_eq!(result.total, 1);
    assert_eq!(result.hits[0].entry_id, "result");
    assert!(search(&file.0, &RecallOptions { page: 2, ..opts() }).is_err());
}
#[test]
fn metadata_that_cannot_fit_errors_without_losing_page_members() {
    let f = Fixture::new(&"x".repeat(2000), vec![msg("x", None, "user", "needle")]);
    assert!(search(
        &f.0,
        &RecallOptions {
            max_output_bytes: 1024,
            ..opts()
        }
    )
    .is_err());
}
