#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use ultracompress_core::transform::{run, TransformInput};
use ultracompress_core::ucbridge::UcBridge;

struct FakeUc(PathBuf);
impl FakeUc {
    fn new(fail_once: bool) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "uc-no-gain-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("uc");
        let fail = if fail_once {
            "if [ ! -e \"$0.failed\" ]; then touch \"$0.failed\"; exit 1; fi"
        } else {
            ""
        };
        std::fs::write(
            &path,
            format!(
                r#"#!/bin/sh
case "$1" in
--version) echo 'uc fixture';;
encode)
  cat >/dev/null
  {fail}
  echo '{{"ok":true}}'
  echo '{{"tokens":{{"uc":9,"jsonMin":9}}}}' >&2
  ;;
count) touch "$0.count"; exit 1;;
*) exit 1;;
esac
"#
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    fn bin(&self) -> &str {
        self.0.to_str().unwrap()
    }
}
impl Drop for FakeUc {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

#[test]
fn no_gain_skips_redundant_tokenizer_and_failures_remain_retryable() {
    let fake = FakeUc::new(true);
    let mut bridge = UcBridge::new(fake.bin());
    assert!(bridge.probe().available);
    let text = "{\"ok\":true}";
    assert!(bridge.encode_json(text).is_none());
    assert!(!bridge.no_gain(text)); // Failed encode is not cached.
    assert!(bridge.encode_json(text).is_none());
    assert!(bridge.no_gain(text)); // A successful codec-j tie is cacheable.
    assert!(bridge.encode_json(text).is_none());
    assert_eq!(bridge.cache_stats(), (2, 1));
    assert!(!fake.0.with_extension("count").exists());
}

#[test]
fn transform_reports_only_successful_no_gain_positions() {
    for fail_once in [false, true] {
        let fake = FakeUc::new(fail_once);
        let text = serde_json::json!({"text": "x".repeat(2000)}).to_string();
        let input: TransformInput = serde_json::from_value(serde_json::json!({
            "messages": [{"role":"toolResult", "content":[{"type":"text", "text":text}]}],
            "policy":"uc", "vision":"off", "uc_bin":fake.bin(), "uc_enabled":true
        }))
        .unwrap();
        let result = run(&input).unwrap();
        assert!(result.ops.is_empty());
        assert_eq!(result.no_gain.len(), usize::from(!fail_once));
        if !fail_once {
            assert_eq!(result.no_gain[0].message_index, 0);
            assert_eq!(result.no_gain[0].block_index, 0);
        }
    }
}
