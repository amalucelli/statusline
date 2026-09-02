use std::io::Write;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_statusline");

#[test]
fn help_succeeds_and_hides_the_refresh_flag() {
    let out = Command::new(BIN).arg("--help").output().unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();

    assert!(out.status.success(), "--help exited {:?}", out.status);
    assert!(!stdout.contains("--refresh-usage"));
}

#[test]
fn version_flag_prints_name_and_version() {
    let out = Command::new(BIN).arg("-v").output().unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();

    assert!(out.status.success());
    assert!(
        stdout.starts_with(concat!("statusline ", env!("CARGO_PKG_VERSION"))),
        "unexpected version output: {stdout:?}"
    );
}

#[test]
fn unknown_flag_is_rejected() {
    let out = Command::new(BIN).arg("--bogus").output().unwrap();

    assert_eq!(out.status.code(), Some(2));
    assert!(!out.stderr.is_empty());
}

// Claude Code invokes the binary with no arguments and reads only stdout, so bad
// input must not surface as an error. The payload fails deserialization on
// purpose: a well-formed one would reach the usage cache, whose paths are
// hardcoded under /tmp and shared with the running statusline.
#[test]
fn unreadable_stdin_exits_zero_without_stderr() {
    let mut child = Command::new(BIN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"not json").unwrap();
    let out = child.wait_with_output().unwrap();

    assert!(out.status.success());
    assert!(
        out.stderr.is_empty(),
        "wrote to stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
