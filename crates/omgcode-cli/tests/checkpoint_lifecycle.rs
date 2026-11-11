//! Real CLI tests for explicit checkpoint restore and deletion.

mod support;

use std::fs;

#[cfg(any(target_os = "linux", windows))]
use serde_json::Value;
use uuid::Uuid;

#[cfg(any(target_os = "linux", windows))]
use support::path_arg;
use support::workspace_fixture::Fixture;
use support::{assert_control_error, assert_error_code, assert_success, json_output, run_cli};

const ORIGINAL: &[u8] = b"before\r\nlast line";
#[cfg(any(target_os = "linux", windows))]
const PROPOSED: &[u8] = b"after\r\nlast line\r\n";

#[cfg(any(target_os = "linux", windows))]
impl Fixture {
    /// Read the current target through the public CLI and return its exact digest.
    fn current_sha256(&self) -> String {
        let output = run_cli(
            &self.data,
            &[
                "file",
                "read",
                &self.workspace_id.to_string(),
                "--session",
                &self.session_id.to_string(),
                "note.txt",
            ],
        );
        assert_success(&output);
        json_output(&output)["sha256"]
            .as_str()
            .expect("current digest")
            .to_owned()
    }

    /// Apply complete proposed bytes and return the new operation ID.
    fn apply_bytes(&self, proposed: &[u8]) -> String {
        let source = self._temporary.path().join("proposed.txt");
        fs::write(&source, proposed).expect("proposed bytes");
        let current = self.current_sha256();
        let bytes = fs::read(self.workspace.join("note.txt"))
            .expect("current bytes")
            .len()
            .to_string();
        let output = run_cli(
            &self.data,
            &[
                "file",
                "apply",
                &self.workspace_id.to_string(),
                "note.txt",
                "--session",
                &self.session_id.to_string(),
                "--content-file",
                path_arg(&source),
                "--expected-sha256",
                &current,
                "--expected-bytes",
                &bytes,
                "--writers-stopped",
            ],
        );
        assert_success(&output);
        json_output(&output)["operation_id"]
            .as_str()
            .expect("operation ID")
            .to_owned()
    }

    /// Run restore with an explicit current-file precondition.
    fn restore(
        &self,
        checkpoint: &str,
        expected_sha256: &str,
        expected_bytes: usize,
        writers_stopped: bool,
    ) -> std::process::Output {
        let workspace_id = self.workspace_id.to_string();
        let session_id = self.session_id.to_string();
        let expected_bytes = expected_bytes.to_string();
        let mut args = vec![
            "file",
            "restore",
            &workspace_id,
            "note.txt",
            "--session",
            &session_id,
            "--checkpoint",
            checkpoint,
            "--expected-sha256",
            expected_sha256,
            "--expected-bytes",
            &expected_bytes,
        ];
        if writers_stopped {
            args.push("--writers-stopped");
        }
        run_cli(&self.data, &args)
    }
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn restore_and_delete_round_trip_through_the_cli() {
    let fixture = Fixture::new(ORIGINAL);
    let target = fixture.workspace.join("note.txt");
    let applied = fixture.apply_bytes(PROPOSED);
    let current = fixture.current_sha256();

    let output = fixture.restore(&applied, &current, PROPOSED.len(), false);
    assert!(!output.status.success(), "missing acknowledgement accepted");
    assert!(String::from_utf8_lossy(&output.stderr).contains("--writers-stopped"));
    let output = fixture.restore(&applied, &"0".repeat(64), PROPOSED.len(), true);
    assert_error_code(&output, "checkpoint_conflict");
    assert_eq!(fs::read(&target).expect("unchanged target"), PROPOSED);

    let output = fixture.restore(&applied, &current, PROPOSED.len(), true);
    assert_success(&output);
    let receipt = json_output(&output);
    assert_eq!(receipt["restored_from"], applied.as_str());
    assert_eq!(receipt["applied"], true);
    assert_eq!(receipt["original"]["sha256"], current.as_str());
    let restored = receipt["operation_id"].as_str().expect("new operation");
    assert_ne!(restored, applied);
    assert_eq!(fs::read(&target).expect("restored bytes"), ORIGINAL);

    let entry = fixture.data.join("replacement-v1").join(&applied);
    let output = run_cli(&fixture.data, &["file", "delete-checkpoint", &applied]);
    assert!(!output.status.success(), "missing acknowledgement accepted");
    assert!(String::from_utf8_lossy(&output.stderr).contains("--permanent"));
    assert!(entry.join("original").exists());

    let output = run_cli(
        &fixture.data,
        &["file", "delete-checkpoint", &applied, "--permanent"],
    );
    assert_success(&output);
    let deletion = json_output(&output);
    assert_eq!(deletion["operation_id"], applied.as_str());
    assert_eq!(deletion["resumed"], false);
    assert!(!entry.exists());
    assert_eq!(fs::read(&target).expect("target untouched"), ORIGINAL);

    let output = run_cli(&fixture.data, &["file", "checkpoints"]);
    assert_success(&output);
    let view = json_output(&output);
    assert_eq!(view["pending"], Value::Null);
    assert_eq!(view["pending_deletion"], Value::Null);
    let operations = view["operations"].as_array().expect("operations");
    assert_eq!(operations.len(), 2);
    assert_eq!(operations[0]["operation_id"], applied.as_str());
    assert_eq!(operations[0]["retention"], "deleted");
    assert_eq!(operations[0]["outcome"], "succeeded");
    assert_eq!(operations[1]["retention"], "retained");

    let restored_sha = fixture.current_sha256();
    let output = fixture.restore(&applied, &restored_sha, ORIGINAL.len(), true);
    assert_error_code(&output, "checkpoint_unavailable");
    let output = run_cli(
        &fixture.data,
        &["file", "delete-checkpoint", &applied, "--permanent"],
    );
    assert_error_code(&output, "checkpoint_unavailable");
}

#[test]
fn deletion_requires_existing_settings_without_creating_them() {
    let temporary = tempfile::tempdir().expect("private fixture root");
    let data = temporary.path().join("absent-application-data");
    let id = Uuid::new_v4().to_string();
    let output = run_cli(&data, &["file", "delete-checkpoint", &id, "--permanent"]);
    assert_control_error(&output);
    assert!(!data.exists());
}

#[test]
fn deleting_from_unused_storage_creates_nothing() {
    let fixture = Fixture::new(ORIGINAL);
    let id = Uuid::new_v4().to_string();
    let output = run_cli(
        &fixture.data,
        &["file", "delete-checkpoint", &id, "--permanent"],
    );
    assert_error_code(&output, "checkpoint_unavailable");
    assert!(!fixture.data.join("replacement-v1").exists());
    assert!(!fixture.data.join("replacement-init-v1.jsonl").exists());
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("unchanged"),
        ORIGINAL
    );
}
