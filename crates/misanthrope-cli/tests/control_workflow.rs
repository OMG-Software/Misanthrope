use serde_json::{Value, json};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use uuid::Uuid;

const CLI_DEADLINE: Duration = Duration::from_secs(10);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_CAPTURE_BYTES: u64 = 1024 * 1024;
const HANG_FIXTURE_ENV: &str = "MISANTHROPE_CONTROL_WORKFLOW_HANG_FIXTURE";

struct BoundedOutput {
    output: Output,
    timed_out: bool,
}

/// Run a child with a deadline and bounded output reads.
///
/// Temporary files avoid a full output pipe blocking the child. Reads are capped at
/// 1 MiB after exit; temporary-file disk growth is not capped. The caller gets the
/// status and both streams only after the child has exited or been killed and reaped.
fn run_with_deadline(mut command: Command, timeout: Duration) -> io::Result<BoundedOutput> {
    let stdout_file = tempfile::tempfile()?;
    let stderr_file = tempfile::tempfile()?;
    let mut stdout_reader = stdout_file.try_clone()?;
    let mut stderr_reader = stderr_file.try_clone()?;
    command
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "process timeout is too large")
    })?;
    let mut child = command.spawn()?;
    let mut timed_out = false;

    let status = loop {
        match child.try_wait() {
            Ok(Some(_)) => break child.wait()?,
            Ok(None) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    timed_out = true;
                    break terminate_and_reap(&mut child)?;
                }
                thread::sleep(remaining.min(PROCESS_POLL_INTERVAL));
            }
            Err(error) => {
                return match terminate_and_reap(&mut child) {
                    Ok(_) => Err(error),
                    Err(cleanup_error) => Err(cleanup_error),
                };
            }
        }
    };

    Ok(BoundedOutput {
        output: Output {
            status,
            stdout: read_capture(&mut stdout_reader)?,
            stderr: read_capture(&mut stderr_reader)?,
        },
        timed_out,
    })
}

/// Kill a running child and wait for its exit status before returning.
fn terminate_and_reap(child: &mut Child) -> io::Result<ExitStatus> {
    match child.kill() {
        Ok(()) => child.wait(),
        Err(kill_error) => match child.try_wait()? {
            Some(_) => child.wait(),
            None => Err(kill_error),
        },
    }
}

/// Read at most the configured capture limit from an exited child output file.
fn read_capture(file: &mut File) -> io::Result<Vec<u8>> {
    if file.metadata()?.len() > MAX_CAPTURE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "child output exceeds the 1 MiB capture limit",
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(MAX_CAPTURE_BYTES + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_CAPTURE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "child output exceeds the 1 MiB capture limit",
        ));
    }
    Ok(bytes)
}

/// Run the CLI with the suite deadline and retain both output streams for assertions.
fn run_cli(data_root: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_misanthrope"));
    command
        .env_remove("MISANTHROPE_LM_TOKEN")
        .env_remove(HANG_FIXTURE_ENV)
        .arg("--data-dir")
        .arg(data_root)
        .args(args);
    let result = run_with_deadline(command, CLI_DEADLINE).expect("run Misanthrope CLI");
    assert!(
        !result.timed_out,
        "CLI exceeded its 10-second deadline: stdout={} stderr={}",
        String::from_utf8_lossy(&result.output.stdout),
        String::from_utf8_lossy(&result.output.stderr)
    );
    result.output
}

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
        .env_remove("MISANTHROPE_LM_TOKEN")
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
fn json_output(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("CLI stdout is JSON")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "CLI failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_control_error(output: &Output) {
    assert!(!output.status.success(), "command unexpectedly succeeded");
    let error: Value = serde_json::from_slice(&output.stderr).expect("CLI error is JSON");
    assert_eq!(error["error"]["code"], "control");
}

fn path_arg(path: &Path) -> &str {
    path.to_str().expect("temporary paths are Unicode")
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

    // When flags are omitted, defaults keep read/write enabled and execute/network disabled.
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
            "task_network": false
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
    mkfifo.arg(&fifo).env_remove("MISANTHROPE_LM_TOKEN");
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
fn capabilities_report_configuration_only_policy_without_execution_tools() {
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
    assert_eq!(capabilities["process_execution"], false);
    assert_eq!(capabilities["sandbox_backend"], "unavailable");
    assert!(!data_root.exists());
}
