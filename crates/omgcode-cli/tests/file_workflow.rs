mod support;

use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use uuid::Uuid;

use support::{
    assert_control_error, assert_error_code, assert_success, json_output, path_arg, run_cli,
};

const FILE_CONTENT: &str = "FILE_BODY_SECRET_9fa0321d2c7e";

struct WorkspaceFixture {
    _temporary: TempDir,
    data_root: PathBuf,
    development_root: PathBuf,
    workspace: PathBuf,
    workspace_id: String,
    session_id: String,
}

/// Create sibling application-data and development trees with one registered workspace.
fn workspace_fixture() -> WorkspaceFixture {
    let temporary = TempDir::new().expect("private temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    let workspace = development_root.join("project");
    fs::create_dir_all(&workspace).expect("create workspace");

    let status = run_cli(&data_root, &["workspace", "status"]);
    assert_success(&status);
    assert_eq!(json_output(&status)["revision"], 0);
    let root = run_cli(
        &data_root,
        &[
            "workspace",
            "set-root",
            path_arg(&development_root),
            "--revision",
            "0",
        ],
    );
    assert_success(&root);
    assert_eq!(json_output(&root)["revision"], 1);
    let registered = run_cli(
        &data_root,
        &[
            "workspace",
            "register",
            path_arg(&workspace),
            "--revision",
            "1",
        ],
    );
    assert_success(&registered);
    let workspace_id = json_output(&registered)["registration_id"]
        .as_str()
        .expect("workspace ID")
        .to_owned();

    WorkspaceFixture {
        _temporary: temporary,
        data_root,
        development_root,
        workspace,
        workspace_id,
        session_id: Uuid::new_v4().to_string(),
    }
}

/// Assert a serialized native path without lossy Unicode conversion.
fn assert_native_path(value: &Value, path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(value["encoding"], "unix_bytes");
        assert_eq!(value["units"], json!(path.as_os_str().as_bytes()));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        assert_eq!(value["encoding"], "windows_utf16");
        assert_eq!(
            value["units"],
            json!(path.as_os_str().encode_wide().collect::<Vec<_>>())
        );
    }
}

/// Run a scoped file command with a fixture's trusted session and workspace IDs.
fn scoped_args<'a>(fixture: &'a WorkspaceFixture, command: &'a str) -> Vec<&'a str> {
    vec![
        "file",
        command,
        &fixture.workspace_id,
        "--session",
        &fixture.session_id,
    ]
}

#[test]
fn cli_creates_lists_reads_and_audits_files_without_recording_contents() {
    // Given a configured workspace with application storage outside its development root.
    let fixture = workspace_fixture();
    assert!(!fixture.data_root.starts_with(&fixture.development_root));
    assert!(!fixture.development_root.starts_with(&fixture.data_root));

    // When the operator creates a directory and a file, then reads and lists them.
    let mut mkdir_args = scoped_args(&fixture, "mkdir");
    mkdir_args.push("drafts");
    let mkdir = run_cli(&fixture.data_root, &mkdir_args);
    assert_success(&mkdir);
    let mkdir = json_output(&mkdir);
    assert_eq!(mkdir["bytes_written"], 0);
    assert_native_path(&mkdir["target"], Path::new("drafts"));

    let mut create_args = scoped_args(&fixture, "create");
    create_args.extend(["drafts/note.txt", "--content", FILE_CONTENT]);
    let create = run_cli(&fixture.data_root, &create_args);
    assert_success(&create);
    let create = json_output(&create);
    assert_eq!(create["bytes_written"], FILE_CONTENT.len());
    assert_native_path(&create["target"], &PathBuf::from("drafts").join("note.txt"));

    let mut read_args = scoped_args(&fixture, "read");
    let max_bytes = (FILE_CONTENT.len() + 1).to_string();
    read_args.extend(["drafts/note.txt", "--max-bytes", &max_bytes]);
    let read = run_cli(&fixture.data_root, &read_args);
    assert_success(&read);
    let read = json_output(&read);
    assert_eq!(read["bytes"], json!(FILE_CONTENT.as_bytes()));
    assert_eq!(
        read["sha256"],
        "6edd82cad271ce6aefc86605f49c990c12e34bd5e7203ae83a3a8626ab91dec7"
    );
    assert_native_path(&read["target"], &PathBuf::from("drafts").join("note.txt"));

    let mut list_args = scoped_args(&fixture, "list");
    list_args.extend(["drafts", "--limit", "8"]);
    let listed = run_cli(&fixture.data_root, &list_args);
    assert_success(&listed);
    let listed = json_output(&listed);
    assert_eq!(listed["truncated"], false);
    assert_native_path(&listed["target"], Path::new("drafts"));
    let entries = listed["entries"].as_array().expect("listed entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["kind"], "file");
    assert_native_path(&entries[0]["name"], Path::new("note.txt"));

    let mut root_list_args = scoped_args(&fixture, "list");
    root_list_args.extend(["--limit", "8"]);
    let root_list = run_cli(&fixture.data_root, &root_list_args);
    assert_success(&root_list);
    let root_list = json_output(&root_list);
    assert_native_path(&root_list["target"], Path::new(""));
    let root_entries = root_list["entries"]
        .as_array()
        .expect("workspace root entries");
    assert_eq!(root_entries.len(), 1);
    assert_eq!(root_entries[0]["kind"], "directory");
    assert_native_path(&root_entries[0]["name"], Path::new("drafts"));

    // When an operator repeats exclusive creation, the original bytes stay intact.
    let mut collision_args = scoped_args(&fixture, "create");
    collision_args.extend(["drafts/note.txt", "--content", "replacement"]);
    let collision = run_cli(&fixture.data_root, &collision_args);
    assert_error_code(&collision, "file_conflict");
    assert_eq!(
        fs::read(fixture.workspace.join("drafts").join("note.txt")).expect("read original file"),
        FILE_CONTENT.as_bytes()
    );

    // Then audit metadata records the operations and does not contain file contents.
    let audit = run_cli(&fixture.data_root, &["file", "audit", "--limit", "20"]);
    assert_success(&audit);
    let audit = json_output(&audit);
    assert_eq!(audit["pending"], Value::Null);
    let entries = audit["entries"].as_array().expect("file audit entries");
    let serialized = serde_json::to_string(entries).expect("serialize audit entries");
    assert!(
        !serialized.contains(FILE_CONTENT),
        "audit must not retain file contents"
    );
    let started: Vec<&Value> = entries
        .iter()
        .filter(|entry| entry["action"]["type"] == "operation_started")
        .collect();
    assert_eq!(started.len(), 6, "audit entries: {entries:#?}");
    assert_eq!(
        started
            .iter()
            .map(|entry| {
                entry["action"]["payload"]["operation"]
                    .as_str()
                    .expect("operation")
            })
            .collect::<Vec<_>>(),
        [
            "create_directory",
            "create_file",
            "read",
            "list",
            "list",
            "create_file"
        ]
    );
    let outcomes: Vec<&str> = entries
        .iter()
        .filter(|entry| entry["action"]["type"] == "operation_finished")
        .map(|entry| {
            entry["action"]["payload"]["outcome"]
                .as_str()
                .expect("outcome")
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            "succeeded",
            "succeeded",
            "succeeded",
            "succeeded",
            "succeeded",
            "failed"
        ]
    );

    // Capabilities keep native file access separate from model tool dispatch.
    let capabilities = run_cli(&fixture.data_root, &["capabilities"]);
    assert_success(&capabilities);
    let capabilities = json_output(&capabilities);
    let native_broker_implemented = cfg!(any(target_os = "linux", target_os = "windows"));
    assert_eq!(
        capabilities["workspace_file_broker_implemented"],
        native_broker_implemented
    );
    let expected_availability = if native_broker_implemented {
        serde_json::Value::Null
    } else {
        serde_json::Value::Bool(false)
    };
    assert_eq!(capabilities["workspace_file_broker"], expected_availability);
    assert_eq!(
        capabilities["exclusive_file_creation"],
        expected_availability
    );
    for flag in [
        "existing_file_replacement",
        "checkpoint_restore",
        "checkpoint_deletion",
    ] {
        assert_eq!(
            capabilities[flag],
            cfg!(any(target_os = "linux", target_os = "windows")),
            "{flag}"
        );
    }
    assert_eq!(capabilities["edit_previews"], native_broker_implemented);
    assert_eq!(capabilities["workspace_file_tools"], false);
}

#[test]
fn current_read_and_write_preferences_deny_file_operations() {
    // Given a workspace with a seed file and both preferences enabled by default.
    let fixture = workspace_fixture();
    let mut create_args = scoped_args(&fixture, "create");
    create_args.extend(["seed.txt", "--content", "original"]);
    let seed = run_cli(&fixture.data_root, &create_args);
    assert_success(&seed);

    // When trusted settings disable read and write at the current revision.
    let permissions = run_cli(
        &fixture.data_root,
        &[
            "workspace",
            "permissions",
            &fixture.workspace_id,
            "--revision",
            "2",
            "--read",
            "false",
            "--write",
            "false",
        ],
    );
    assert_success(&permissions);
    assert_eq!(json_output(&permissions)["revision"], 3);

    // Then new reads and writes are denied, and the existing file remains unchanged.
    let mut read_args = scoped_args(&fixture, "read");
    read_args.push("seed.txt");
    assert_error_code(&run_cli(&fixture.data_root, &read_args), "file_denied");
    let mut write_args = scoped_args(&fixture, "create");
    write_args.extend(["new.txt", "--content", "denied"]);
    assert_error_code(&run_cli(&fixture.data_root, &write_args), "file_denied");
    assert_eq!(
        fs::read(fixture.workspace.join("seed.txt")).expect("seed still exists"),
        b"original"
    );
    assert!(!fixture.workspace.join("new.txt").exists());

    let audit = run_cli(&fixture.data_root, &["file", "audit"]);
    assert_success(&audit);
    let audit = json_output(&audit);
    let entries = audit["entries"].as_array().expect("audit entries");
    let starts = entries
        .iter()
        .filter(|entry| entry["action"]["type"] == "operation_started")
        .count();
    assert_eq!(starts, 1, "denied operations must not start file I/O");
}

#[test]
fn traversal_and_absolute_targets_cannot_reach_outside_files() {
    // Given a sentinel outside the registered workspace.
    let fixture = workspace_fixture();
    let sentinel = fixture.development_root.join("sentinel.txt");
    fs::write(&sentinel, b"outside").expect("write outside sentinel");

    // When the operator supplies traversal and absolute targets, both are rejected.
    for target in ["../sentinel.txt", path_arg(&sentinel)] {
        let mut args = scoped_args(&fixture, "read");
        args.push(target);
        assert_error_code(&run_cli(&fixture.data_root, &args), "file_request");
    }

    // Then the sentinel remains unchanged.
    assert_eq!(fs::read(&sentinel).expect("read sentinel"), b"outside");
}

#[test]
fn file_commands_fail_closed_without_store_or_configured_workspace() {
    // Given no application store, file audit reports a controlled configuration error.
    let temporary = TempDir::new().expect("private temporary directory");
    let data_root = temporary.path().join("application-data");
    assert_control_error(&run_cli(&data_root, &["file", "audit"]));
    assert!(
        !data_root.exists(),
        "inspection must not initialize settings"
    );

    // When settings exist but have no development root or workspace, scoped I/O is denied.
    let status = run_cli(&data_root, &["workspace", "status"]);
    assert_success(&status);
    let workspace_id = Uuid::new_v4().to_string();
    let session_id = Uuid::new_v4().to_string();
    assert_control_error(&run_cli(
        &data_root,
        &["file", "list", &workspace_id, "--session", &session_id],
    ));

    // Invalid byte limits fail as controlled request errors before any I/O.
    let configured = workspace_fixture();
    let mut invalid_read = scoped_args(&configured, "read");
    invalid_read.extend(["absent.txt", "--max-bytes", "0"]);
    assert_error_code(
        &run_cli(&configured.data_root, &invalid_read),
        "file_request",
    );

    // An acknowledgement for an unknown operation cannot change audit state.
    let unknown = Uuid::new_v4().to_string();
    assert_error_code(
        &run_cli(&configured.data_root, &["file", "acknowledge", &unknown]),
        "file_audit",
    );
}

/// Recovery commands remain usable without a selected workspace after a root change.
#[test]
fn cli_archives_completed_audit_and_inspects_it_after_workspace_selection_changes() {
    let fixture = workspace_fixture();
    fs::write(fixture.workspace.join("entry.txt"), b"kept").expect("fixture file");
    let mut args = scoped_args(&fixture, "read");
    args.push("entry.txt");
    assert_success(&run_cli(&fixture.data_root, &args));
    let before = fs::read(fixture.data_root.join("file-operations-v1.jsonl")).expect("old log");
    let archives = fixture._temporary.path().join("operator-private-archives");
    fs::create_dir(&archives).expect("archive directory");
    let next_root = fixture._temporary.path().join("next-development-root");
    fs::create_dir(&next_root).expect("new root");
    assert_success(&run_cli(
        &fixture.data_root,
        &[
            "workspace",
            "set-root",
            path_arg(&next_root),
            "--revision",
            "2",
        ],
    ));

    let rotated = run_cli(
        &fixture.data_root,
        &["file", "rotate", "--archive-dir", path_arg(&archives)],
    );
    assert_success(&rotated);
    let receipt = json_output(&rotated);
    let native: omgcode_core::session::NativePath =
        serde_json::from_value(receipt["archive"].clone()).expect("lossless archive path");
    let path = native.to_path_buf().expect("same host");
    assert_eq!(receipt["event_count"], 3);
    assert_eq!(fs::read(&path).expect("archive retained"), before);
    let inspected = run_cli(
        &fixture.data_root,
        &["file", "audit", "--archive", path_arg(&path)],
    );
    assert_success(&inspected);
    assert_eq!(
        json_output(&inspected)["entries"]
            .as_array()
            .expect("events")
            .len(),
        3
    );
    let active = run_cli(&fixture.data_root, &["file", "audit"]);
    assert_success(&active);
    assert_eq!(
        json_output(&active)["entries"]
            .as_array()
            .expect("fresh header")
            .len(),
        1
    );
    assert_eq!(
        fs::read(&path).expect("read-only archive inspection"),
        before
    );
    let missing = archives.join("missing.jsonl");
    assert_error_code(
        &run_cli(
            &fixture.data_root,
            &["file", "audit", "--archive", path_arg(&missing)],
        ),
        "file_audit",
    );
    assert!(!missing.exists());
}

#[test]
fn cli_previews_a_bounded_diff_without_modifying_the_workspace() {
    let fixture = workspace_fixture();
    let mut create_args = scoped_args(&fixture, "create");
    create_args.extend(["note.txt", "--content", "one\ntwo\nthree\n"]);
    assert_success(&run_cli(&fixture.data_root, &create_args));

    let proposed = fixture._temporary.path().join("proposed.txt");
    fs::write(&proposed, "one\nTWO\nthree\n").expect("write proposed content");
    let mut diff_args = scoped_args(&fixture, "diff");
    diff_args.extend(["note.txt", "--content-file", path_arg(&proposed)]);
    let diff = run_cli(&fixture.data_root, &diff_args);
    assert_success(&diff);
    let diff = json_output(&diff);
    assert_eq!(diff["changed"], true);
    assert_eq!(diff["truncated"], false);
    assert_eq!(
        diff["diff"],
        "--- current\n+++ proposed\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n"
    );
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("workspace file"),
        b"one\ntwo\nthree\n"
    );

    // The preview performs a broker read but no workspace mutation.
    let audit = run_cli(&fixture.data_root, &["file", "audit", "--limit", "20"]);
    assert_success(&audit);
    let audit = json_output(&audit);
    let started: Vec<&Value> = audit["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .filter(|entry| entry["action"]["type"] == "operation_started")
        .collect();
    assert_eq!(started.len(), 2);
    assert_eq!(started[0]["action"]["payload"]["operation"], "create_file");
    assert_eq!(started[1]["action"]["payload"]["operation"], "read");
}

#[test]
fn edit_preview_fails_closed_on_denial_and_invalid_content() {
    let fixture = workspace_fixture();
    let mut create_args = scoped_args(&fixture, "create");
    create_args.extend(["note.txt", "--content", "original\n"]);
    assert_success(&run_cli(&fixture.data_root, &create_args));

    let proposed = fixture._temporary.path().join("proposed.txt");
    fs::write(&proposed, "changed\n").expect("write proposed content");

    // When read is revoked, the preview is denied before file-operation audit.
    let permissions = run_cli(
        &fixture.data_root,
        &[
            "workspace",
            "permissions",
            &fixture.workspace_id,
            "--revision",
            "2",
            "--read",
            "false",
        ],
    );
    assert_success(&permissions);
    let mut denied_args = scoped_args(&fixture, "diff");
    denied_args.extend(["note.txt", "--content-file", path_arg(&proposed)]);
    assert_error_code(&run_cli(&fixture.data_root, &denied_args), "file_denied");
    let audit = run_cli(&fixture.data_root, &["file", "audit"]);
    assert_success(&audit);
    assert_eq!(
        json_output(&audit)["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .filter(|entry| entry["action"]["type"] == "operation_started")
            .count(),
        1,
        "only the initial create may be recorded"
    );

    // Re-enable read for the content-validation cases.
    let permissions = run_cli(
        &fixture.data_root,
        &[
            "workspace",
            "permissions",
            &fixture.workspace_id,
            "--revision",
            "3",
            "--read",
            "true",
        ],
    );
    assert_success(&permissions);

    let binary = fixture._temporary.path().join("binary.bin");
    fs::write(&binary, [0xff, 0xfe]).expect("write binary proposal");
    let mut binary_args = scoped_args(&fixture, "diff");
    binary_args.extend(["note.txt", "--content-file", path_arg(&binary)]);
    assert_error_code(&run_cli(&fixture.data_root, &binary_args), "edit_text");

    let large = fixture._temporary.path().join("large.txt");
    fs::write(&large, "x".repeat(64)).expect("write oversized proposal");
    let mut large_args = scoped_args(&fixture, "diff");
    large_args.extend([
        "note.txt",
        "--content-file",
        path_arg(&large),
        "--max-bytes",
        "32",
    ]);
    assert_error_code(&run_cli(&fixture.data_root, &large_args), "edit_limit");

    // The current-side size limit is enforced by the broker and reports file_io.
    fs::write(fixture.workspace.join("oversized.txt"), "x".repeat(64))
        .expect("write oversized current file");
    let mut current_args = scoped_args(&fixture, "diff");
    current_args.extend([
        "oversized.txt",
        "--content-file",
        path_arg(&proposed),
        "--max-bytes",
        "32",
    ]);
    assert_error_code(&run_cli(&fixture.data_root, &current_args), "file_io");

    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("workspace file"),
        b"original\n"
    );
}

#[cfg(unix)]
#[test]
fn edit_preview_rejects_a_symlinked_proposed_source() {
    use std::os::unix::fs::symlink;

    let fixture = workspace_fixture();
    let mut create_args = scoped_args(&fixture, "create");
    create_args.extend(["note.txt", "--content", "original\n"]);
    assert_success(&run_cli(&fixture.data_root, &create_args));

    let target = fixture._temporary.path().join("proposed-target.txt");
    let link = fixture._temporary.path().join("proposed-link.txt");
    fs::write(&target, "changed\n").expect("write proposed target");
    symlink(&target, &link).expect("create proposed symlink");
    let mut diff_args = scoped_args(&fixture, "diff");
    diff_args.extend(["note.txt", "--content-file", path_arg(&link)]);

    assert_error_code(&run_cli(&fixture.data_root, &diff_args), "edit_request");
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("workspace file"),
        b"original\n"
    );
}
