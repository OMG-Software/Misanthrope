//! Shared bounded subprocess and JSON helpers for real CLI workflow tests.

#[allow(
    dead_code,
    reason = "only the run workflow and model command test binaries use the run scene"
)]
pub(crate) mod run_scene;
pub(crate) mod spawn_gate;
#[allow(
    dead_code,
    reason = "only checkpointed file test binaries register workspaces"
)]
pub(crate) mod workspace_fixture;

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Maximum time allowed for a normal CLI fixture process.
pub(crate) const CLI_DEADLINE: Duration = Duration::from_secs(10);
/// Environment variable that enables the timeout fixture in the test binary.
pub(crate) const HANG_FIXTURE_ENV: &str = "OMGCODE_CONTROL_WORKFLOW_HANG_FIXTURE";
/// A variable set on every CLI process, so tests can prove that a confined run
/// does not inherit the caller's environment.
pub(crate) const HOST_ENV_SENTINEL: &str = "OMGCODE_TEST_HOST_ENV";
pub(crate) const HOST_ENV_SENTINEL_VALUE: &str = "host-env-sentinel-5b81";
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_CAPTURE_BYTES: u64 = 1024 * 1024;

/// Completed child output plus whether the deadline forced termination.
pub(crate) struct BoundedOutput {
    pub(crate) output: Output,
    pub(crate) timed_out: bool,
}

/// Run a child to completion with a deadline and bounded output reads.
///
/// Temporary files avoid a full output pipe blocking the child. Reads are capped
/// at 1 MiB after exit; temporary-file disk growth is not capped.
pub(crate) fn run_with_deadline(
    mut command: Command,
    timeout: Duration,
) -> io::Result<BoundedOutput> {
    let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "process timeout is too large")
    })?;
    let stdout_file = tempfile::tempfile()?;
    let stderr_file = tempfile::tempfile()?;
    let mut stdout_reader = stdout_file.try_clone()?;
    let mut stderr_reader = stderr_file.try_clone()?;
    command
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    let mut child = spawn_gate::spawn(&mut command)?;
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

/// Terminate a child and always attempt to reap it.
fn terminate_and_reap(child: &mut Child) -> io::Result<ExitStatus> {
    match child.kill() {
        Ok(()) => child.wait(),
        Err(kill_error) => match child.try_wait() {
            Ok(Some(_)) => child.wait(),
            Ok(None) => Err(kill_error),
            Err(wait_error) => {
                // The child may have exited between kill and this check. Try
                // wait even when that status check fails so cleanup is observed.
                match child.wait() {
                    Ok(status) => Ok(status),
                    Err(_) => Err(wait_error),
                }
            }
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

/// Run the CLI with isolated token state and the workflow deadline.
pub(crate) fn run_cli(data_root: &Path, args: &[&str]) -> Output {
    run_cli_for(data_root, args, CLI_DEADLINE)
}

/// Run the CLI with isolated token state and a chosen deadline.
pub(crate) fn run_cli_for(data_root: &Path, args: &[&str], deadline: Duration) -> Output {
    let result =
        run_with_deadline(cli_command(data_root, args), deadline).expect("run OMGCode CLI");
    assert!(
        !result.timed_out,
        "CLI exceeded its {deadline:?} deadline: stdout={} stderr={}",
        String::from_utf8_lossy(&result.output.stdout),
        String::from_utf8_lossy(&result.output.stderr)
    );
    result.output
}

/// The CLI command with isolated token state, before it is started.
pub(crate) fn cli_command(data_root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_omgcode"));
    command
        .env_remove("OMGCODE_LM_TOKEN")
        .env_remove(HANG_FIXTURE_ENV)
        .env(HOST_ENV_SENTINEL, HOST_ENV_SENTINEL_VALUE)
        .arg("--data-dir")
        .arg(data_root)
        .args(args);
    command
}

/// Parse stdout as the CLI's one-line JSON result.
pub(crate) fn json_output(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).expect("CLI stdout is JSON")
}

/// Require successful exit and show both captured output streams on failure.
pub(crate) fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "CLI failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Require a controlled JSON error with the selected stable category.
#[allow(dead_code, reason = "the toolchain acceptance binary checks no errors")]
pub(crate) fn assert_error_code(output: &Output, code: &str) {
    assert!(!output.status.success(), "command unexpectedly succeeded");
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("CLI error is JSON");
    assert_eq!(error["error"]["code"], code);
}

/// Require a control-store error from a trusted settings command.
#[allow(dead_code, reason = "the toolchain acceptance binary checks no errors")]
pub(crate) fn assert_control_error(output: &Output) {
    assert_error_code(output, "control");
}

/// Return a UTF-8 argument for a test-created native path.
pub(crate) fn path_arg(path: &Path) -> &str {
    path.to_str().expect("temporary paths are Unicode")
}
