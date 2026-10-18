use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "audiomix-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let script = path.join("pactl");
        fs::write(
            &script,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$AUDIOMIX_TEST_LOG"
case "$*" in
  '-f json list sinks') cat "$AUDIOMIX_FIXTURES/sinks.json" ;;
  '-f json list modules') cat "$AUDIOMIX_FIXTURES/modules.json" ;;
  'list short modules') cat "$AUDIOMIX_FIXTURES/short_modules.txt" ;;
  '-f json list cards') cat "$AUDIOMIX_FIXTURES/cards.json" ;;
  'get-default-sink') printf '%s\n' audiomix_sala ;;
  *) printf '%s\n' 'Forbidden unexpected backend command' >&2; exit 64 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }

    fn invoke(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_audiomix"))
            .args(args)
            .env("AUDIOMIX_PACTL", self.0.join("pactl"))
            .env("AUDIOMIX_TEST_LOG", self.0.join("calls"))
            .env(
                "AUDIOMIX_FIXTURES",
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"),
            )
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn assert_no_mutation(&self) {
        let calls = fs::read_to_string(self.0.join("calls")).unwrap_or_default();
        for call in calls.lines() {
            assert!(
                call.starts_with("-f json list ")
                    || call == "list short modules"
                    || call == "get-default-sink",
                "Unexpected mutation: {call}"
            );
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn cli_refuses_non_terminal_without_subcommand() {
    let test = Sandbox::new();
    let output = test.invoke(&[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--help"));
    test.assert_no_mutation();
}

#[test]
fn cli_lists_fixture_as_valid_json() {
    let test = Sandbox::new();
    let output = test.invoke(&["list", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 6);
    assert!(rows
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["default"] == true));
    test.assert_no_mutation();
}

#[test]
fn cli_rejects_unsafe_requests_without_backend_mutation() {
    for args in [
        vec!["combine", "--name", "bad name", "10", "12"],
        vec!["combine", "--name", "test", "10", "alsa_output.pci-analog"],
        vec!["combine", "--name", "test", "10", "11"],
        vec!["combine", "--name", "test", "10", "15"],
        vec!["remove", "legado"],
        vec!["remove", "sala", "--fallback", "audiomix_sala"],
        vec!["stereo", "10"],
    ] {
        let test = Sandbox::new();
        let output = test.invoke(&args);
        assert!(!output.status.success(), "Unexpected success: {args:?}");
        assert!(!output.stderr.is_empty(), "Missing diagnostic: {args:?}");
        test.assert_no_mutation();
    }
}

#[test]
fn cli_missing_backend_has_actionable_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_audiomix"))
        .args(["list"])
        .env("AUDIOMIX_PACTL", "/nonexistent-audiomix-backend/pactl")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("pactl"));
}
