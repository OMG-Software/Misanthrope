//! Real CLI tests for trusted checkpointed replacement.

mod support;

use std::fs;
use std::path::Path;

#[cfg(target_os = "linux")]
use omgcode_core::control::ControlStore;
#[cfg(target_os = "linux")]
use omgcode_core::policy::PermissionSet;
#[cfg(target_os = "linux")]
use serde_json::Value;
#[cfg(target_os = "linux")]
use serde_json::json;
use uuid::Uuid;

#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;

#[cfg(target_os = "linux")]
use support::spawn_gate::without_child_spawns;
use support::workspace_fixture::Fixture;
use support::{
    assert_control_error, assert_error_code, assert_success, json_output, path_arg, run_cli,
};

const ORIGINAL: &[u8] = b"before\r\nlast line";
#[cfg(target_os = "linux")]
const PROPOSED: &[u8] = b"after\r\nlast line\r\n";

impl Fixture {
    /// Read the current target through the public CLI and return its exact digest.
    #[cfg(target_os = "linux")]
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

    /// Run the replacement route with or without its required operator acknowledgement.
    fn apply(
        &self,
        source: &Path,
        expected_sha256: &str,
        expected_bytes: usize,
        writers_stopped: bool,
    ) -> std::process::Output {
        let workspace_id = self.workspace_id.to_string();
        let session_id = self.session_id.to_string();
        let expected_bytes = expected_bytes.to_string();
        let mut args = vec![
            "file",
            "apply",
            &workspace_id,
            "note.txt",
            "--session",
            &session_id,
            "--content-file",
            path_arg(source),
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

/// Read sorted names to detect unexpected checkpoint-store initialization.
fn child_names(directory: &Path) -> Vec<std::ffi::OsString> {
    let mut names = fs::read_dir(directory)
        .expect("fixture directory")
        .map(|entry| entry.expect("directory entry").file_name())
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn apply_requires_existing_settings_without_creating_them() {
    let temporary = tempfile::tempdir().expect("private fixture root");
    let data = temporary.path().join("absent-application-data");
    let workspace_id = Uuid::new_v4().to_string();
    let session_id = Uuid::new_v4().to_string();
    let expected_sha256 = "0".repeat(64);

    let output = run_cli(
        &data,
        &[
            "file",
            "apply",
            &workspace_id,
            "note.txt",
            "--session",
            &session_id,
            "--content-file",
            "missing-proposed.txt",
            "--expected-sha256",
            &expected_sha256,
            "--expected-bytes",
            "1",
            "--writers-stopped",
        ],
    );
    assert_control_error(&output);
    assert!(!data.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn apply_replaces_exact_bytes_and_retains_a_completed_checkpoint() {
    let fixture = Fixture::new(ORIGINAL);
    let proposed_file = fixture._temporary.path().join("proposed.txt");
    fs::write(&proposed_file, PROPOSED).expect("complete proposed bytes");
    let expected_sha256 = fixture.current_sha256();
    let original_metadata =
        fs::metadata(fixture.workspace.join("note.txt")).expect("original file metadata");

    let output = fixture.apply(&proposed_file, &expected_sha256, ORIGINAL.len(), true);
    assert_success(&output);
    let receipt = json_output(&output);
    assert_eq!(receipt["changed"], true);
    assert_eq!(receipt["applied"], true);
    assert_eq!(receipt["original"]["bytes"], json!(ORIGINAL.len()));
    assert_eq!(receipt["original"]["sha256"], expected_sha256);
    assert_eq!(receipt["proposed"]["bytes"], json!(PROPOSED.len()));
    let readback = run_cli(
        &fixture.data,
        &[
            "file",
            "read",
            &fixture.workspace_id.to_string(),
            "--session",
            &fixture.session_id.to_string(),
            "note.txt",
        ],
    );
    assert_success(&readback);
    assert_eq!(
        receipt["proposed"]["sha256"],
        json_output(&readback)["sha256"]
    );
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("published bytes"),
        PROPOSED
    );
    let published_metadata =
        fs::metadata(fixture.workspace.join("note.txt")).expect("published file metadata");
    assert_ne!(original_metadata.ino(), published_metadata.ino());

    let operation_id = receipt["operation_id"]
        .as_str()
        .expect("checkpoint operation ID");
    let checkpoints = run_cli(&fixture.data, &["file", "checkpoints"]);
    assert_success(&checkpoints);
    let view = json_output(&checkpoints);
    assert_eq!(view["state"], "ready");
    assert_eq!(view["pending"], Value::Null);
    assert_eq!(view["operations"].as_array().expect("operations").len(), 1);
    assert_eq!(
        fs::read(
            fixture
                .data
                .join("replacement-v1")
                .join(operation_id)
                .join("original")
        )
        .expect("retained original checkpoint"),
        ORIGINAL
    );
    assert_eq!(
        fs::read(
            fixture
                .data
                .join("replacement-v1")
                .join(operation_id)
                .join("proposed")
        )
        .expect("retained proposed checkpoint"),
        PROPOSED
    );
}

#[cfg(target_os = "linux")]
#[test]
fn applying_identical_bytes_does_not_create_a_checkpoint() {
    let fixture = Fixture::new(ORIGINAL);
    let proposed_file = fixture._temporary.path().join("proposed.txt");
    fs::write(&proposed_file, ORIGINAL).expect("same proposed bytes");
    let expected_sha256 = fixture.current_sha256();
    let before_names = child_names(&fixture.data);

    let output = fixture.apply(&proposed_file, &expected_sha256, ORIGINAL.len(), true);
    assert_success(&output);
    let receipt = json_output(&output);
    assert_eq!(receipt["operation_id"], Value::Null);
    assert_eq!(receipt["changed"], false);
    assert_eq!(receipt["applied"], false);
    assert_eq!(child_names(&fixture.data), before_names);
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("unchanged source"),
        ORIGINAL
    );
}

#[cfg(target_os = "linux")]
#[test]
fn stale_expected_digest_refuses_without_changing_target_or_checkpoint_storage() {
    let fixture = Fixture::new(ORIGINAL);
    let proposed_file = fixture._temporary.path().join("proposed.txt");
    fs::write(&proposed_file, PROPOSED).expect("complete proposed bytes");

    let output = fixture.apply(&proposed_file, &"0".repeat(64), ORIGINAL.len(), true);
    assert_error_code(&output, "checkpoint_conflict");
    assert!(!fixture.data.join("replacement-v1").exists());
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("unchanged source"),
        ORIGINAL
    );
}

#[cfg(target_os = "linux")]
#[test]
fn denied_write_preference_refuses_replacement() {
    let fixture = Fixture::new(ORIGINAL);
    let proposed_file = fixture._temporary.path().join("proposed.txt");
    fs::write(&proposed_file, PROPOSED).expect("complete proposed bytes");
    without_child_spawns(|| {
        let mut control = ControlStore::open_existing(&fixture.data)
            .expect("existing settings")
            .expect("control store exists");
        control
            .set_permissions(
                fixture.workspace_id,
                PermissionSet {
                    read: true,
                    write: false,
                    execute: false,
                    task_network: false,
                },
                control.revision(),
            )
            .expect("revoke write preference");
    });

    let output = fixture.apply(&proposed_file, &"0".repeat(64), ORIGINAL.len(), true);
    assert_error_code(&output, "checkpoint_denied");
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("unchanged source"),
        ORIGINAL
    );
    assert!(!fixture.data.join("replacement-v1").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn apply_requires_explicit_stop_writers_flag_without_side_effects() {
    let fixture = Fixture::new(ORIGINAL);
    let proposed_file = fixture._temporary.path().join("proposed.txt");
    fs::write(&proposed_file, PROPOSED).expect("complete proposed bytes");
    let before_names = child_names(&fixture.data);

    let output = fixture.apply(&proposed_file, &"0".repeat(64), ORIGINAL.len(), false);
    assert!(
        !output.status.success(),
        "missing acknowledgement was accepted"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("--writers-stopped"));
    assert_eq!(child_names(&fixture.data), before_names);
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("unchanged source"),
        ORIGINAL
    );
}

#[cfg(target_os = "linux")]
#[test]
fn apply_rejects_a_symlinked_proposed_source() {
    let fixture = Fixture::new(ORIGINAL);
    let real_source = fixture._temporary.path().join("real-proposed.txt");
    let linked_source = fixture._temporary.path().join("linked-proposed.txt");
    fs::write(&real_source, PROPOSED).expect("complete proposed bytes");
    std::os::unix::fs::symlink(&real_source, &linked_source).expect("proposed symlink");
    let expected_sha256 = fixture.current_sha256();

    let output = fixture.apply(&linked_source, &expected_sha256, ORIGINAL.len(), true);
    assert_error_code(&output, "edit_request");
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("unchanged source"),
        ORIGINAL
    );
    assert!(!fixture.data.join("replacement-v1").exists());
}

#[cfg(windows)]
#[test]
fn windows_apply_refuses_before_checkpoint_or_workspace_mutation() {
    let fixture = Fixture::new(ORIGINAL);
    let absent_source = fixture._temporary.path().join("source-does-not-exist.txt");
    let before_names = child_names(&fixture.data);

    let checkpoints = run_cli(&fixture.data, &["file", "checkpoints"]);
    assert_success(&checkpoints);
    assert_eq!(json_output(&checkpoints)["state"], "unused");

    let output = fixture.apply(&absent_source, &"0".repeat(64), ORIGINAL.len(), true);
    assert_error_code(&output, "file_unsupported");
    assert_eq!(child_names(&fixture.data), before_names);
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("unchanged source"),
        ORIGINAL
    );
}
