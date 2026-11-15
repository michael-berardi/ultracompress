use std::io::Write;
use std::process::{Command, Output, Stdio};

fn transform(vision: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ultracompress"))
        .args(["transform", "--vision", vision])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Invalid options can terminate before reading stdin.
    let _ = child.stdin.take().unwrap().write_all(br#"{"messages":[]}"#);
    child.wait_with_output().unwrap()
}

#[test]
fn invalid_vision_is_rejected_instead_of_enabling_auto() {
    let output = transform("typo");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("bad --vision (auto|on|off)"));
}

#[test]
fn valid_vision_modes_remain_case_insensitive() {
    for mode in ["auto", "on", "off", "AUTO", "ON", "OFF"] {
        assert!(transform(mode).status.success(), "{mode}");
    }
}
