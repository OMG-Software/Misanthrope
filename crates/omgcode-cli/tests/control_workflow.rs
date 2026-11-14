mod support;

use serde_json::{Value, json};
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;
use uuid::Uuid;

#[cfg(unix)]
use support::CLI_DEADLINE;
use support::{
    HANG_FIXTURE_ENV, assert_control_error, assert_error_code, assert_success, json_output,
    path_arg, run_cli, run_with_deadline,
};

/// Park only when the timeout regression launches this test in a child process.
#[test]
fn process_timeout_fixture() {
    if std::env::var_os(HANG_FIXTURE_ENV).is_some() {
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(b"HANG_FIXTURE_READY\n")
            .expect("write child readiness marker");
        stdout.flush().expect("flush child readiness marker");
        loop {
            thread::park();
        }
    }
}
/// Verify that the deadline kills and reaps a child that does not exit.
#[test]
fn process_deadline_kills_and_reaps_a_hung_child() {
    // Given a fixture child that parks indefinitely after the test harness starts it.
    let executable = std::env::current_exe().expect("current test executable");
    let mut command = Command::new(executable);
    command
        .env_remove("OMGCODE_LM_TOKEN")
        .env(HANG_FIXTURE_ENV, "1")
        .args(["process_timeout_fixture", "--nocapture"]);

    // When the deadline expires, the helper kills and waits for the child process.
    let result =
        run_with_deadline(command, Duration::from_secs(3)).expect("run bounded hanging fixture");
    assert!(result.timed_out, "the fixture must exceed its deadline");
    assert!(
        String::from_utf8_lossy(&result.output.stdout).contains("HANG_FIXTURE_READY"),
        "the child must reach its parked state before timeout"
    );
    assert!(
        !result.output.status.success(),
        "the killed child must report a non-success exit"
    );
}
fn set_root(data_root: &Path, development_root: &Path) -> Value {
    let output = run_cli(
        data_root,
        &[
            "workspace",
            "set-root",
            path_arg(development_root),
            "--revision",
            "0",
        ],
    );
    assert_success(&output);
    json_output(&output)
}

#[test]
fn workspace_commands_persist_cas_permissions_policy_and_audit_results() {
    // Given private application data adjacent to, but outside, the development root.
    let temporary = TempDir::new().expect("private temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    let first_workspace = development_root.join("first");
    let second_workspace = development_root.join("second");
    fs::create_dir_all(&first_workspace).expect("create first workspace");
    fs::create_dir_all(&second_workspace).expect("create second workspace");

    // When status initializes an empty store, the first root update advances revision 0 to 1.
    let status = run_cli(&data_root, &["workspace", "status"]);
    assert_success(&status);
    let initial = json_output(&status);
    assert_eq!(initial["revision"], 0);
    assert_eq!(initial["development_root"], Value::Null);
    let root_snapshot = set_root(&data_root, &development_root);
    assert_eq!(root_snapshot["revision"], 1);
    assert_ne!(root_snapshot["development_root"], Value::Null);

    // When two paths register, their IDs persist and an idempotent repeat does not advance revision.
    let first = run_cli(
        &data_root,
        &[
            "workspace",
            "register",
            path_arg(&first_workspace),
            "--revision",
            "1",
        ],
    );
    assert_success(&first);
    let first = json_output(&first);
    let first_id = first["registration_id"]
        .as_str()
        .expect("first workspace ID");
    assert_eq!(first["snapshot"]["revision"], 2);
    let first_repeat = run_cli(
        &data_root,
        &[
            "workspace",
            "register",
            path_arg(&first_workspace),
            "--revision",
            "2",
        ],
    );
    assert_success(&first_repeat);
    let first_repeat = json_output(&first_repeat);
    assert_eq!(first_repeat["registration_id"], first_id);
    assert_eq!(first_repeat["snapshot"]["revision"], 2);

    let second = run_cli(
        &data_root,
        &[
            "workspace",
            "register",
            path_arg(&second_workspace),
            "--revision",
            "2",
        ],
    );
    assert_success(&second);
    let second = json_output(&second);
    let second_id = second["registration_id"]
        .as_str()
        .expect("second workspace ID");
    assert_ne!(first_id, second_id);
    assert_eq!(second["snapshot"]["revision"], 3);

    // When flags are omitted, defaults keep read/write enabled and execute,
    // network, model tools, and host services disabled. Status reports every
    // flag explicitly.
    let second_permissions = second["snapshot"]["workspaces"]
        .as_array()
        .expect("workspace registrations")
        .iter()
        .find(|workspace| workspace["id"] == second_id)
        .expect("second workspace registration");
    let default_permissions = &second_permissions["permissions"];
    assert_eq!(
        default_permissions,
        &json!({
            "read": true,
            "write": true,
            "execute": false,
            "task_network": false,
            "model_tools": false,
            "host_services": false
        })
    );

    // When a caller supplies an obsolete revision, the CLI rejects the update.
    let permissions = run_cli(
        &data_root,
        &[
            "workspace",
            "permissions",
            first_id,
            "--revision",
            "3",
            "--execute",
            "true",
            "--task-network",
            "true",
        ],
    );
    assert_success(&permissions);
    let permissions = json_output(&permissions);
    assert_eq!(permissions["revision"], 4);
    let stale = run_cli(
        &data_root,
        &[
            "workspace",
            "permissions",
            first_id,
            "--revision",
            "3",
            "--read",
            "false",
        ],
    );
    assert_control_error(&stale);

    // When policy checks run, default read/write are eligible while execute/network stay disabled.
    let default_session = Uuid::new_v4().to_string();
    for (operation, eligible) in [
        ("read", true),
        ("write", true),
        ("execute", false),
        ("task-network", false),
    ] {
        let check = run_cli(
            &data_root,
            &[
                "workspace",
                "check",
                second_id,
                "--session",
                &default_session,
                "--operation",
                operation,
            ],
        );
        assert_success(&check);
        let decision = json_output(&check)["decision"].clone();
        assert_eq!(decision["configured_allowed"], eligible);
        assert_eq!(decision["execution_allowed"], false);
    }

    let session_id = Uuid::new_v4().to_string();
    // When policy checks run, an explicitly enabled flag remains configuration eligibility only.
    for (operation, eligible) in [
        ("read", true),
        ("write", true),
        ("execute", true),
        ("task-network", true),
    ] {
        let check = run_cli(
            &data_root,
            &[
                "workspace",
                "check",
                first_id,
                "--session",
                &session_id,
                "--operation",
                operation,
            ],
        );
        assert_success(&check);
        let decision = json_output(&check)["decision"].clone();
        assert_eq!(decision["configured_allowed"], eligible);
        assert_eq!(decision["execution_allowed"], false);
    }

    // When a valid binding targets another registered workspace, policy denies the scope mismatch.
    let cross_workspace = run_cli(
        &data_root,
        &[
            "workspace",
            "check",
            first_id,
            "--session",
            &session_id,
            "--operation",
            "read",
            "--target-workspace",
            second_id,
        ],
    );
    assert_success(&cross_workspace);
    let decision = json_output(&cross_workspace)["decision"].clone();
    assert_eq!(decision["configured_allowed"], false);
    assert_eq!(decision["reason"], "workspace_mismatch");
    assert_eq!(decision["execution_allowed"], false);

    // When the operator requests recent audit history, checks appear in the durable log.
    let audit = run_cli(&data_root, &["workspace", "audit", "--limit", "20"]);
    assert_success(&audit);
    let entries = json_output(&audit)["audit"]
        .as_array()
        .expect("audit entries")
        .clone();
    let policy_checks = entries
        .iter()
        .filter(|entry| entry["action"]["type"] == "policy_checked")
        .count();
    assert_eq!(
        policy_checks, 9,
        "expected four default, four enabled, and one cross-workspace check; entries: {entries:#?}"
    );
}

#[test]
fn configured_sessions_keep_workspace_ids_and_fail_closed_outside_the_root() {
    // Given application data and a development root that do not overlap.
    let temporary = TempDir::new().expect("private temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    let workspace = development_root.join("project");
    let outside_workspace = temporary.path().join("outside");
    fs::create_dir_all(&workspace).expect("create workspace");
    fs::create_dir(&outside_workspace).expect("create outside workspace");
    set_root(&data_root, &development_root);
    let registration = run_cli(
        &data_root,
        &[
            "workspace",
            "register",
            path_arg(&workspace),
            "--revision",
            "1",
        ],
    );
    assert_success(&registration);
    let expected_id = json_output(&registration)["registration_id"].clone();

    // When new sessions use the same configured workspace, they record its stable ID.
    let first = run_cli(
        &data_root,
        &["session", "new", "--workspace", path_arg(&workspace)],
    );
    let second = run_cli(
        &data_root,
        &["session", "new", "--workspace", path_arg(&workspace)],
    );
    assert_success(&first);
    assert_success(&second);
    assert_eq!(json_output(&first)["session"]["workspace_id"], expected_id);
    assert_eq!(json_output(&second)["session"]["workspace_id"], expected_id);

    // When session creation targets an unregistered path outside the root, it fails before journaling.
    let outside = run_cli(
        &data_root,
        &[
            "session",
            "new",
            "--workspace",
            path_arg(&outside_workspace),
        ],
    );
    assert_control_error(&outside);
    let journal_dir = data_root.join("sessions");
    let journal_count = fs::read_dir(&journal_dir)
        .expect("session directory exists")
        .flat_map(|year| {
            fs::read_dir(year.expect("year directory").path()).expect("month directory")
        })
        .flat_map(|month| fs::read_dir(month.expect("month entry").path()).expect("day directory"))
        .flat_map(|day| fs::read_dir(day.expect("day entry").path()).expect("journal files"))
        .count();
    assert_eq!(journal_count, 2, "outside session must not be written");
}

/// Verify Unix FIFO paths fail promptly and release the settings-store lock.
#[cfg(unix)]
#[test]
fn fifo_paths_fail_promptly_and_release_the_control_store() {
    // Given a FIFO below a valid development root and private application data.
    let temporary = TempDir::new().expect("private temporary directory");
    let development_root = temporary.path().join("development");
    fs::create_dir(&development_root).expect("create development root");
    let fifo = development_root.join("workspace-fifo");
    let mut mkfifo = Command::new("mkfifo");
    mkfifo.arg(&fifo).env_remove("OMGCODE_LM_TOKEN");
    let created = run_with_deadline(mkfifo, CLI_DEADLINE).expect("run mkfifo fixture command");
    assert!(!created.timed_out, "mkfifo fixture command must finish");
    assert_success(&created.output);

    // When the FIFO is selected as the root, the CLI must return a controlled failure.
    let root_data = temporary.path().join("root-application-data");
    let root_attempt = run_cli(
        &root_data,
        &["workspace", "set-root", path_arg(&fifo), "--revision", "0"],
    );
    assert_control_error(&root_attempt);
    let after_root_failure = run_cli(&root_data, &["workspace", "status"]);
    assert_success(&after_root_failure);
    let after_root_failure = json_output(&after_root_failure);
    assert_eq!(after_root_failure["revision"], 0);
    assert_eq!(after_root_failure["development_root"], Value::Null);

    // When the valid root is configured, registering the FIFO also fails and releases the lock.
    let registration_data = temporary.path().join("registration-application-data");
    let root_snapshot = set_root(&registration_data, &development_root);
    assert_eq!(root_snapshot["revision"], 1);
    let registration_attempt = run_cli(
        &registration_data,
        &["workspace", "register", path_arg(&fifo), "--revision", "1"],
    );
    assert_control_error(&registration_attempt);
    let after_registration_failure = run_cli(&registration_data, &["workspace", "status"]);
    assert_success(&after_registration_failure);
    let after_registration_failure = json_output(&after_registration_failure);
    assert_eq!(after_registration_failure["revision"], 1);
    assert_ne!(after_registration_failure["development_root"], Value::Null);
}
#[test]
fn corrupt_control_state_fails_closed_for_new_sessions() {
    // Given a private application-data directory with a corrupt control journal.
    let temporary = TempDir::new().expect("private temporary directory");
    let data_root = temporary.path().join("application-data");
    fs::create_dir_all(&data_root).expect("create application data");
    fs::write(
        data_root.join("control-v1.jsonl"),
        b"not a valid control journal\n",
    )
    .expect("write corrupt control state");
    let workspace = temporary.path().join("workspace");
    fs::create_dir(&workspace).expect("create workspace");

    // When a session tries to open that state, it reports control failure without fallback.
    let output = run_cli(
        &data_root,
        &["session", "new", "--workspace", path_arg(&workspace)],
    );
    assert_control_error(&output);
    assert!(!data_root.join("sessions").exists());
}

#[test]
fn legacy_session_creation_remains_available_without_a_control_store() {
    // Given a private data root with no control store, legacy text sessions still work.
    let temporary = TempDir::new().expect("private temporary directory");
    let data_root = temporary.path().join("application-data");
    let workspace = temporary.path().join("workspace");
    fs::create_dir(&workspace).expect("create workspace");

    // When a text session is created, it records the legacy per-session workspace ID.
    let output = run_cli(
        &data_root,
        &["session", "new", "--workspace", path_arg(&workspace)],
    );
    assert_success(&output);
    let output = json_output(&output);
    assert!(output["session"]["workspace_id"].as_str().is_some());
}

#[test]
fn capabilities_report_configuration_only_policy_and_trusted_runs_only() {
    // Given a private data directory that the capabilities command does not need to open.
    let temporary = TempDir::new().expect("private temporary directory");
    let data_root = temporary.path().join("application-data");

    // When capabilities are requested, policy configuration is reported separately from runners.
    let output = run_cli(&data_root, &["capabilities"]);
    assert_success(&output);
    let capabilities = json_output(&output);
    assert_eq!(capabilities["workspace_registry"], true);
    assert_eq!(capabilities["permission_policy"], true);
    assert_eq!(capabilities["permission_audit"], true);
    assert_eq!(capabilities["policy_decision"], "configuration_only");
    assert_eq!(capabilities["workspace_file_tools"], false);
    assert_eq!(capabilities["workspace_container_grants"], cfg!(windows));
    let runs = cfg!(any(target_os = "linux", windows));
    assert_eq!(capabilities["toolchain_profiles"], runs);
    assert_eq!(capabilities["model_command_tool"], "approval_required");
    assert_eq!(capabilities["model_write_tools"], false);
    assert_eq!(capabilities["package_network_profiles"], true);
    assert_eq!(capabilities["package_network_enforcement"], false);
    assert_eq!(capabilities["package_fetch"], true);
    assert_eq!(capabilities["provider_profiles"], true);
    assert_eq!(capabilities["provider_adapters"], false);
    assert_eq!(capabilities["approvals"], true);
    if runs {
        assert_eq!(capabilities["process_execution"], "trusted_cli_only");
    } else {
        assert_eq!(capabilities["process_execution"], false);
    }
    let backend = if cfg!(target_os = "linux") {
        "landlock"
    } else if cfg!(windows) {
        "appcontainer"
    } else {
        "unavailable"
    };
    assert_eq!(capabilities["sandbox_backend"], backend);
    assert!(!data_root.exists());
}

/// Package-network profiles are recorded with revisions but never authorize a
/// run. Invalid origins and duplicates are refused without a revision change.
#[test]
fn package_profiles_are_configuration_only_and_revisioned() {
    let temporary = TempDir::new().expect("temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    fs::create_dir_all(&development_root).expect("development root");
    let snapshot = set_root(&data_root, &development_root);
    let revision = snapshot["revision"].as_u64().expect("revision");

    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "package-profile-add",
            "crates",
            "--registry",
            "https://registry.example.com/api/v4",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_success(&output);
    let added = json_output(&output);
    assert!(added["package_profile_id"].as_str().is_some());
    assert_eq!(added["snapshot"]["package_profiles"][0]["name"], "crates");
    let revision = added["snapshot"]["revision"]
        .as_u64()
        .expect("revision after add");

    let output = run_cli(&data_root, &["workspace", "package-profile-show", "CRATES"]);
    assert_success(&output);
    let shown = json_output(&output);
    assert_eq!(shown["package_profile"]["name"], "crates");
    assert_eq!(shown["package_profile"]["origins"][0]["scheme"], "https");
    assert_eq!(
        shown["package_profile"]["origins"][0]["host"],
        "registry.example.com"
    );
    assert_eq!(shown["package_profile"]["origins"][0]["port"], 443);
    assert_eq!(
        shown["package_profile"]["origins"][0]["path_prefix"],
        "/api/v4/"
    );

    let output = run_cli(&data_root, &["workspace", "status"]);
    assert_success(&output);
    assert_eq!(
        json_output(&output)["package_profiles"][0]["name"],
        "crates"
    );

    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "package-profile-add",
            "plain",
            "--registry",
            "http://registry.example.com/",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_control_error(&output);
    let output = run_cli(&data_root, &["workspace", "status"]);
    assert_eq!(
        json_output(&output)["revision"],
        Value::from(revision),
        "a refused profile changed the revision"
    );

    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "package-profile-add",
            "CRATES",
            "--registry",
            "https://other.example.com/",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_control_error(&output);

    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "package-profile-remove",
            "crates",
            "--revision",
            "0",
        ],
    );
    assert_control_error(&output);

    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "package-profile-remove",
            "crates",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_success(&output);
    assert!(
        json_output(&output)["snapshot"]
            .get("package_profiles")
            .is_none()
    );

    let output = run_cli(&data_root, &["workspace", "package-profile-show", "crates"]);
    assert_control_error(&output);
}

/// Provider profiles record only a connection shape and authentication mode;
/// they hold no credential and enable no adapter.
#[test]
fn provider_profiles_are_configuration_only_and_revisioned() {
    let temporary = TempDir::new().expect("temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    fs::create_dir_all(&development_root).expect("development root");
    let revision = set_root(&data_root, &development_root)["revision"]
        .as_u64()
        .expect("revision");

    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "provider-profile-add",
            "work",
            "--kind",
            "openai",
            "--auth",
            "subscription",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_success(&output);
    let added = json_output(&output);
    assert!(added["provider_profile_id"].as_str().is_some());
    assert_eq!(added["snapshot"]["provider_profiles"][0]["name"], "work");
    assert_eq!(added["snapshot"]["provider_profiles"][0]["kind"], "openai");
    assert_eq!(
        added["snapshot"]["provider_profiles"][0]["auth"],
        "subscription"
    );
    let revision = added["snapshot"]["revision"]
        .as_u64()
        .expect("revision after add");

    let output = run_cli(&data_root, &["workspace", "provider-profile-show", "WORK"]);
    assert_success(&output);
    let shown = json_output(&output);
    assert_eq!(shown["provider_profile"]["name"], "work");
    assert_eq!(shown["provider_profile"]["kind"], "openai");
    assert_eq!(shown["provider_profile"]["auth"], "subscription");

    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "provider-profile-add",
            "invalid",
            "--kind",
            "ollama_local",
            "--auth",
            "api_key",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_control_error(&output);
    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "provider-profile-add",
            "WORK",
            "--kind",
            "anthropic",
            "--auth",
            "api_key",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_control_error(&output);

    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "provider-profile-remove",
            "work",
            "--revision",
            "0",
        ],
    );
    assert_control_error(&output);
    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "provider-profile-remove",
            "work",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_success(&output);
    assert!(
        json_output(&output)["snapshot"]
            .get("provider_profiles")
            .is_none()
    );
    let output = run_cli(&data_root, &["workspace", "provider-profile-show", "work"]);
    assert_control_error(&output);
}

/// Approvals are requested, decided, and consumed once; a denied or replayed
/// request is refused. No operation executes.
#[test]
fn approvals_are_requested_decided_and_consumed_once() {
    let temporary = TempDir::new().expect("temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    let workspace = development_root.join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let revision = set_root(&data_root, &development_root)["revision"]
        .as_u64()
        .expect("revision");
    let workspace_id = register(&data_root, &workspace, revision);
    let session = Uuid::new_v4().to_string();
    let digest = "a".repeat(64);

    let output = run_cli(
        &data_root,
        &[
            "approvals",
            "request",
            "--session",
            &session,
            "--workspace",
            &workspace_id,
            "--operation",
            "command",
            "--digest",
            &digest,
            "--summary",
            "Run the project build",
            "--ttl-secs",
            "900",
        ],
    );
    assert_success(&output);
    let approval = json_output(&output)["approval"].clone();
    let approval_id = approval["id"].as_str().expect("approval id").to_owned();
    assert_eq!(approval["state"], "pending");

    let output = run_cli(&data_root, &["approvals", "list"]);
    assert_success(&output);
    assert_eq!(json_output(&output)["approvals"][0]["id"], approval_id);

    let output = run_cli(&data_root, &["approvals", "approve", &approval_id]);
    assert_success(&output);
    assert_eq!(json_output(&output)["decision"], "approved");
    assert_eq!(json_output(&output)["approval"]["state"], "approved");

    let output = run_cli(
        &data_root,
        &[
            "approvals",
            "consume",
            &approval_id,
            "--session",
            &session,
            "--workspace",
            &workspace_id,
            "--operation",
            "command",
            "--digest",
            &digest,
        ],
    );
    assert_success(&output);
    assert_eq!(json_output(&output)["approval"]["state"], "consumed");
    let output = run_cli(
        &data_root,
        &[
            "approvals",
            "consume",
            &approval_id,
            "--session",
            &session,
            "--workspace",
            &workspace_id,
            "--operation",
            "command",
            "--digest",
            &digest,
        ],
    );
    assert_error_code(&output, "approval");

    let denied_digest = "b".repeat(64);
    let output = run_cli(
        &data_root,
        &[
            "approvals",
            "request",
            "--session",
            &session,
            "--workspace",
            &workspace_id,
            "--operation",
            "command",
            "--digest",
            &denied_digest,
            "--summary",
            "Second request",
            "--ttl-secs",
            "900",
        ],
    );
    assert_success(&output);
    let denied_id = json_output(&output)["approval"]["id"]
        .as_str()
        .expect("denied id")
        .to_owned();
    let output = run_cli(&data_root, &["approvals", "deny", &denied_id]);
    assert_success(&output);
    assert_eq!(json_output(&output)["decision"], "denied");
    let output = run_cli(
        &data_root,
        &[
            "approvals",
            "consume",
            &denied_id,
            "--session",
            &session,
            "--workspace",
            &workspace_id,
            "--operation",
            "command",
            "--digest",
            &denied_digest,
        ],
    );
    assert_error_code(&output, "approval");

    let output = run_cli(
        &data_root,
        &[
            "approvals",
            "request",
            "--session",
            &session,
            "--workspace",
            &workspace_id,
            "--operation",
            "command",
            "--digest",
            "not-a-digest",
            "--summary",
            "Invalid",
        ],
    );
    assert_error_code(&output, "approval");
    let missing = Uuid::new_v4();
    let output = run_cli(&data_root, &["approvals", "show", &missing.to_string()]);
    assert_error_code(&output, "approval");
}

fn register(data_root: &Path, workspace: &Path, revision: u64) -> String {
    let output = run_cli(
        data_root,
        &[
            "workspace",
            "register",
            path_arg(workspace),
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_success(&output);
    json_output(&output)["registration_id"]
        .as_str()
        .expect("registration ID")
        .to_owned()
}

#[cfg(windows)]
#[test]
fn container_grants_are_recorded_listed_and_revoked() {
    // Given a registered workspace and a toolchain directory outside the root.
    let temporary = TempDir::new().expect("private temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    let workspace = development_root.join("app");
    let toolchain = temporary.path().join("toolchain");
    fs::create_dir_all(&workspace).expect("create workspace");
    fs::create_dir_all(&toolchain).expect("create toolchain");
    set_root(&data_root, &development_root);
    let id = register(&data_root, &workspace, 1);

    // When the workspace and the toolchain are granted to its container.
    let write = run_cli(
        &data_root,
        &[
            "workspace",
            "container-grant",
            &id,
            path_arg(&workspace),
            "--access",
            "read-write",
            "--revision",
            "2",
        ],
    );
    assert_success(&write);
    let write_id = json_output(&write)["grant_id"]
        .as_str()
        .expect("grant ID")
        .to_owned();
    let refused = run_cli(
        &data_root,
        &[
            "workspace",
            "container-grant",
            &id,
            path_arg(&toolchain),
            "--access",
            "read-write",
            "--revision",
            "3",
        ],
    );
    assert_control_error(&refused);
    let read = run_cli(
        &data_root,
        &[
            "workspace",
            "container-grant",
            &id,
            path_arg(&toolchain),
            "--access",
            "read-execute",
            "--revision",
            "3",
        ],
    );
    assert_success(&read);
    let read_id = json_output(&read)["grant_id"]
        .as_str()
        .expect("grant ID")
        .to_owned();

    // Then status lists both as active, with the workspace container.
    let status = run_cli(&data_root, &["workspace", "status"]);
    assert_success(&status);
    let status = json_output(&status);
    assert_eq!(status["revision"], 4);
    let grants = status["container_grants"].as_array().expect("grants");
    assert_eq!(grants.len(), 2);
    for grant in grants {
        assert_eq!(grant["state"], "active");
        assert_eq!(grant["workspace_id"], id.as_str());
        let container = grant["container"].as_str().expect("container");
        assert_eq!(
            container,
            format!("OMGCode.Workspace.{}", id.replace('-', ""))
        );
        assert!(grant["sid"].as_str().expect("SID").starts_with("S-1-15-2-"));
    }

    // And revocation needs no revision; the last one deletes the profile.
    let first = run_cli(&data_root, &["workspace", "container-revoke", &write_id]);
    assert_success(&first);
    assert_eq!(json_output(&first)["revocation"]["profile_deleted"], false);
    let last = run_cli(&data_root, &["workspace", "container-revoke", &read_id]);
    assert_success(&last);
    let last = json_output(&last);
    assert_eq!(last["revocation"]["profile_deleted"], true);
    assert_eq!(last["snapshot"]["revision"], 4);
    assert!(last["snapshot"].get("container_grants").is_none());
    let again = run_cli(&data_root, &["workspace", "container-revoke", &read_id]);
    assert_control_error(&again);
}

#[cfg(not(windows))]
#[test]
fn container_grants_are_refused_without_appcontainer_support() {
    let temporary = TempDir::new().expect("private temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    let workspace = development_root.join("app");
    fs::create_dir_all(&workspace).expect("create workspace");
    set_root(&data_root, &development_root);
    let id = register(&data_root, &workspace, 1);
    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "container-grant",
            &id,
            path_arg(&workspace),
            "--access",
            "read-write",
            "--revision",
            "2",
        ],
    );
    assert_control_error(&output);
    let status = run_cli(&data_root, &["workspace", "status"]);
    assert_success(&status);
    let status = json_output(&status);
    assert_eq!(status["revision"], 2);
    assert!(status.get("container_grants").is_none());
}
