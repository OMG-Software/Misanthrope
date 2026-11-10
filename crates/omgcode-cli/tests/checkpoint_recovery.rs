//! Trusted CLI inspection and acknowledgement never apply checkpoint bytes.

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use omgcode_core::control::ControlStore;
#[cfg(target_os = "linux")]
use omgcode_core::files::{FileBroker, FileTarget};
use omgcode_core::policy::{PermissionSet, WorkspaceId};
#[cfg(target_os = "linux")]
use omgcode_core::replacement::RecordedArtifact;
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

use support::{
    assert_control_error, assert_error_code, assert_success, json_output, path_arg, run_cli,
};

const ORIGINAL: &[u8] = b"private_original_payload_9fb26b\n";
#[cfg(target_os = "linux")]
const PROPOSED: &[u8] = b"private_proposed_payload_a827d4\n";

struct Fixture {
    _temporary: TempDir,
    data: PathBuf,
    workspace: PathBuf,
    workspace_id: WorkspaceId,
    session_id: Uuid,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("private fixture root");
        let data = temporary.path().join("application-data");
        let development = temporary.path().join("development");
        let workspace = development.join("project");
        fs::create_dir_all(&workspace).expect("workspace directories");
        fs::write(workspace.join("note.txt"), ORIGINAL).expect("original workspace file");

        let mut control = ControlStore::open(&data).expect("private control store");
        control
            .set_development_root(&development, control.revision())
            .expect("development root");
        let workspace_id = control
            .register_workspace(&workspace, control.revision())
            .expect("registered workspace")
            .id();
        control
            .set_permissions(
                workspace_id,
                PermissionSet {
                    read: true,
                    write: true,
                    execute: false,
                    task_network: false,
                },
                control.revision(),
            )
            .expect("explicit file preferences");

        Self {
            _temporary: temporary,
            data,
            workspace,
            workspace_id,
            session_id: Uuid::new_v4(),
        }
    }

    fn read(&self) -> std::process::Output {
        run_cli(
            &self.data,
            &[
                "file",
                "read",
                &self.workspace_id.to_string(),
                "--session",
                &self.session_id.to_string(),
                "note.txt",
            ],
        )
    }

    #[cfg(target_os = "linux")]
    fn prepare(&self) -> Uuid {
        let mut control = ControlStore::open_existing(&self.data)
            .expect("existing settings")
            .expect("control store exists");
        let binding = control
            .bind_session(self.session_id, self.workspace_id)
            .expect("live binding");
        let target = FileTarget {
            session_id: self.session_id,
            workspace_id: self.workspace_id,
            path: PathBuf::from("note.txt"),
        };
        let mut broker = FileBroker::new(&mut control);
        let current = broker
            .read(&binding, &target, 1024)
            .expect("current exact bytes");
        let expected = RecordedArtifact {
            bytes: u64::try_from(current.bytes.len()).expect("bounded current size"),
            sha256: current.sha256,
        };
        broker
            .checkpoint_prepare(&binding, &target, &expected, PROPOSED)
            .expect("private checkpoint preparation")
            .operation_id
    }
}

fn child_names(directory: &Path) -> Vec<std::ffi::OsString> {
    let mut names = fs::read_dir(directory)
        .expect("fixture directory")
        .map(|entry| entry.expect("directory entry").file_name())
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn checkpoint_recovery_requires_existing_settings_without_initializing_them() {
    let temporary = tempfile::tempdir().expect("private fixture root");
    let data = temporary.path().join("absent-application-data");
    assert_control_error(&run_cli(&data, &["file", "checkpoints"]));
    assert_control_error(&run_cli(
        &data,
        &[
            "file",
            "acknowledge-checkpoint",
            &Uuid::new_v4().to_string(),
        ],
    ));
    assert!(!data.exists());
}

#[test]
fn unused_checkpoint_inspection_and_refused_acknowledgement_create_no_storage() {
    let fixture = Fixture::new();
    let before_names = child_names(&fixture.data);
    let before_settings = fs::read(fixture.data.join("control-v1.jsonl")).expect("settings bytes");

    let output = run_cli(&fixture.data, &["file", "checkpoints"]);
    assert_success(&output);
    let view = json_output(&output);
    assert_eq!(view["state"], "unused");
    assert!(view["control_store_id"].as_str().is_some());
    assert_eq!(view["replacement_store_id"], Value::Null);
    assert_eq!(view["pending"], Value::Null);
    assert_eq!(view["operations"], json!([]));
    assert_eq!(view["managed_bytes"], 0);

    let acknowledgement = run_cli(
        &fixture.data,
        &[
            "file",
            "acknowledge-checkpoint",
            &Uuid::new_v4().to_string(),
        ],
    );
    assert_error_code(&acknowledgement, "checkpoint_request");
    assert_eq!(child_names(&fixture.data), before_names);
    assert_eq!(
        fs::read(fixture.data.join("control-v1.jsonl")).expect("settings"),
        before_settings
    );
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("source"),
        ORIGINAL
    );
    let read = fixture.read();
    assert_success(&read);
    assert_eq!(json_output(&read)["bytes"], json!(ORIGINAL));
}

#[test]
fn checkpoint_recovery_commands_do_not_add_prepare_apply_or_restore_routes() {
    let fixture = Fixture::new();
    let before = child_names(&fixture.data);
    for command in ["prepare", "apply", "restore"] {
        let output = run_cli(
            &fixture.data,
            &["file", command, path_arg(&fixture.workspace)],
        );
        assert!(!output.status.success(), "unexpected file {command} route");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("unrecognized subcommand"),
            "unexpected refusal for file {command}"
        );
    }
    assert_eq!(child_names(&fixture.data), before);
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("source"),
        ORIGINAL
    );
}

#[cfg(target_os = "linux")]
#[test]
fn pending_checkpoint_blocks_file_reads_until_trusted_acknowledgement_without_application() {
    let fixture = Fixture::new();
    let operation_id = fixture.prepare();
    let before_settings = fs::read(fixture.data.join("control-v1.jsonl")).expect("settings bytes");
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("source after preparation"),
        ORIGINAL
    );

    let blocked = fixture.read();
    assert_error_code(&blocked, "checkpoint_pending");
    let inspection = run_cli(&fixture.data, &["file", "checkpoints"]);
    assert_success(&inspection);
    let view = json_output(&inspection);
    assert_eq!(view["state"], "ready");
    assert_eq!(view["pending"], json!(operation_id));
    assert_eq!(
        view["operations"]
            .as_array()
            .expect("operation metadata")
            .len(),
        1
    );
    assert!(view["managed_bytes"].as_u64().expect("managed byte count") > 0);
    let rendered = String::from_utf8(inspection.stdout).expect("inspection UTF-8");
    for payload in [ORIGINAL, PROPOSED] {
        let text = std::str::from_utf8(payload).expect("fixture text");
        assert!(!rendered.contains(text.trim_end()));
        assert!(!rendered.contains(&serde_json::to_string(payload).expect("byte-array rendering")));
    }

    let acknowledgement = run_cli(
        &fixture.data,
        &["file", "acknowledge-checkpoint", &operation_id.to_string()],
    );
    assert_success(&acknowledgement);
    assert_eq!(
        json_output(&acknowledgement),
        json!({
            "acknowledged_checkpoint": operation_id, "replayed": false, "applied": false
        })
    );
    let after = run_cli(&fixture.data, &["file", "checkpoints"]);
    assert_success(&after);
    assert_eq!(json_output(&after)["pending"], Value::Null);
    assert_eq!(
        json_output(&after)["operations"]
            .as_array()
            .expect("retained metadata")
            .len(),
        1
    );
    let read = fixture.read();
    assert_success(&read);
    assert_eq!(json_output(&read)["bytes"], json!(ORIGINAL));
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("unchanged source"),
        ORIGINAL
    );
    assert_eq!(
        fs::read(fixture.data.join("control-v1.jsonl")).expect("settings"),
        before_settings
    );
    let entry = fixture
        .data
        .join("replacement-v1")
        .join(operation_id.to_string());
    assert_eq!(
        fs::read(entry.join("original")).expect("retained original"),
        ORIGINAL
    );
    assert_eq!(
        fs::read(entry.join("proposed")).expect("retained proposal"),
        PROPOSED
    );
}

#[cfg(target_os = "linux")]
#[test]
fn checkpoint_acknowledgement_does_not_restore_revoked_workspace_permissions() {
    let fixture = Fixture::new();
    let operation_id = fixture.prepare();
    let mut control = ControlStore::open_existing(&fixture.data)
        .expect("existing settings")
        .expect("control store exists");
    control
        .set_permissions(
            fixture.workspace_id,
            PermissionSet {
                read: false,
                write: false,
                execute: false,
                task_network: false,
            },
            control.revision(),
        )
        .expect("permission changes remain available while a checkpoint is pending");
    drop(control);

    let acknowledgement = run_cli(
        &fixture.data,
        &["file", "acknowledge-checkpoint", &operation_id.to_string()],
    );
    assert_success(&acknowledgement);
    assert_error_code(&fixture.read(), "file_denied");
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("unchanged source"),
        ORIGINAL
    );
}

#[cfg(windows)]
#[test]
fn present_checkpoint_storage_refuses_an_unqualified_backend_without_mutation() {
    let fixture = Fixture::new();
    let storage = fixture.data.join("replacement-v1");
    fs::create_dir(&storage).expect("existing unsupported storage marker");
    let before = child_names(&fixture.data);
    let settings = fs::read(fixture.data.join("control-v1.jsonl")).expect("settings bytes");

    let inspection = run_cli(&fixture.data, &["file", "checkpoints"]);
    assert_error_code(&inspection, "checkpoint_unsupported");
    let acknowledgement = run_cli(
        &fixture.data,
        &[
            "file",
            "acknowledge-checkpoint",
            &Uuid::new_v4().to_string(),
        ],
    );
    assert_error_code(&acknowledgement, "checkpoint_unsupported");
    assert_eq!(child_names(&fixture.data), before);
    assert!(child_names(&storage).is_empty());
    assert_eq!(
        fs::read(fixture.data.join("control-v1.jsonl")).expect("settings"),
        settings
    );
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("source"),
        ORIGINAL
    );
}
