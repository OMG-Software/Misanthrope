//! A registered workspace scene for `run` workflow and acceptance tests.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

use super::{
    assert_success, cli_command, json_output, path_arg, run_cli, run_cli_for, run_with_deadline,
};

/// CLI deadline for a container grant or revocation.
const GRANT_DEADLINE: Duration = Duration::from_secs(300);
/// CLI deadline for a settings command that can withdraw a run: the
/// command's own 60-second wait for affected runs, plus room for startup,
/// its lock waits, and recording the change.
pub(crate) const SETTINGS_DEADLINE: Duration = Duration::from_secs(90);

/// A development root with one registered workspace whose `execute`
/// preference is on, and, on Windows, a read-write grant on the workspace.
///
/// End each test with [`Scene::close`], which revokes every grant and fails
/// the test if a revocation fails. `Drop` is only a fallback for a test that
/// already failed: it tries the same revocations and reports, never panics.
pub(crate) struct Scene {
    pub(crate) temporary: TempDir,
    pub(crate) data: PathBuf,
    pub(crate) workspace: PathBuf,
    pub(crate) id: String,
    pub(crate) grants: Vec<String>,
}

impl Scene {
    pub(crate) fn new() -> Self {
        Self::build(true, true)
    }

    pub(crate) fn build(execute: bool, grant: bool) -> Self {
        let temporary = TempDir::new().expect("temporary directory");
        let data = temporary.path().join("data");
        let root = temporary.path().join("dev");
        let workspace = root.join("work space");
        fs::create_dir_all(&workspace).expect("workspace");
        let mut scene = Self {
            data,
            workspace,
            id: String::new(),
            grants: Vec::new(),
            temporary,
        };
        assert_success(&scene.cli(&["workspace", "set-root", path_arg(&root), "--revision", "0"]));
        let registered = scene.cli(&[
            "workspace",
            "register",
            path_arg(&scene.workspace.clone()),
            "--revision",
            "1",
        ]);
        assert_success(&registered);
        scene.id = json_output(&registered)["registration_id"]
            .as_str()
            .expect("workspace id")
            .to_owned();
        if execute {
            scene.set_permission("--execute", "true");
        }
        if grant && cfg!(windows) {
            let workspace = scene.workspace.clone();
            scene.grant(&workspace, "read-write");
        }
        scene
    }

    pub(crate) fn cli(&self, args: &[&str]) -> Output {
        run_cli(&self.data, args)
    }

    /// Grant the workspace container access to a directory and revoke it
    /// when the scene ends.
    pub(crate) fn grant(&mut self, path: &Path, access: &str) {
        // A grant on a large toolchain tree updates every inherited ACL.
        let output = run_cli_for(
            &self.data,
            &[
                "workspace",
                "container-grant",
                &self.id,
                path_arg(path),
                "--access",
                access,
                "--revision",
                &self.revision().to_string(),
            ],
            GRANT_DEADLINE,
        );
        if !output.status.success() {
            // A failed command can still leave a pending grant whose ACL
            // entry exists. Track every grant of this workspace so the
            // scene revokes it, then fail.
            self.track_recorded_grants();
            assert_success(&output);
        }
        self.grants.push(
            json_output(&output)["grant_id"]
                .as_str()
                .expect("grant id")
                .to_owned(),
        );
    }

    /// Add every recorded grant of this workspace that is not yet tracked.
    fn track_recorded_grants(&mut self) {
        let output = self.cli(&["workspace", "status"]);
        if !output.status.success() {
            return;
        }
        let status = json_output(&output);
        for grant in status["container_grants"].as_array().into_iter().flatten() {
            if grant["workspace_id"].as_str() == Some(self.id.as_str())
                && let Some(id) = grant["id"].as_str()
                && !self.grants.iter().any(|known| known == id)
            {
                self.grants.push(id.to_owned());
            }
        }
    }

    /// Revoke every grant and fail on any revocation that fails.
    pub(crate) fn close(mut self) {
        let failures = self.revoke_grants();
        assert!(failures.is_empty(), "grant revocation failed: {failures:?}");
    }

    /// Revoke each tracked grant once, returning the failures. Revoking the
    /// last grant also deletes the container profile, so test runs leave no
    /// per-user AppContainer state behind. A grant whose revocation failed
    /// stays tracked, so `Drop` retries it after `close` fails.
    fn revoke_grants(&mut self) -> Vec<String> {
        let mut failures = Vec::new();
        let mut unresolved = Vec::new();
        for grant in std::mem::take(&mut self.grants) {
            let command = cli_command(&self.data, &["workspace", "container-revoke", &grant]);
            let failure = match run_with_deadline(command, GRANT_DEADLINE) {
                Ok(result) if result.timed_out => Some("timed out".to_owned()),
                Ok(result) if !result.output.status.success() => {
                    Some(String::from_utf8_lossy(&result.output.stderr).into_owned())
                }
                Ok(_) => None,
                Err(error) => Some(error.to_string()),
            };
            if let Some(failure) = failure {
                failures.push(format!("{grant}: {failure}"));
                unresolved.push(grant);
            }
        }
        self.grants = unresolved;
        failures
    }

    pub(crate) fn revision(&self) -> u64 {
        let output = self.cli(&["workspace", "status"]);
        assert_success(&output);
        json_output(&output)["revision"].as_u64().expect("revision")
    }

    /// Change one preference. The deadline covers the command's wait for
    /// the runs that the change withdraws.
    pub(crate) fn set_permission(&self, flag: &str, value: &str) {
        let revision = self.revision().to_string();
        assert_success(&self.settings_cli(&[
            "workspace",
            "permissions",
            &self.id,
            flag,
            value,
            "--revision",
            &revision,
        ]));
    }

    /// Run a settings command that can withdraw runs.
    pub(crate) fn settings_cli(&self, args: &[&str]) -> Output {
        run_cli_for(&self.data, args, SETTINGS_DEADLINE)
    }

    /// Record the system profile, plus any extra roots.
    pub(crate) fn add_profile(&self, name: &str, extra_roots: &[&Path]) -> Output {
        let system = system_root();
        let search = system_path();
        let revision = self.revision().to_string();
        let mut args = vec![
            "workspace",
            "profile-add",
            name,
            "--root",
            path_arg(&system),
            "--path",
            path_arg(&search),
        ];
        for root in extra_roots {
            args.extend(["--root", path_arg(root)]);
        }
        args.extend(["--revision", &revision]);
        self.cli(&args)
    }

    /// Record a profile with explicit roots, PATH entries, and variables.
    pub(crate) fn add_profile_with(
        &self,
        name: &str,
        roots: &[PathBuf],
        path_entries: &[PathBuf],
        environment: &[(String, String)],
    ) -> Output {
        let revision = self.revision().to_string();
        let variables: Vec<String> = environment
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        let mut args = vec!["workspace", "profile-add", name];
        for root in roots {
            args.extend(["--root", path_arg(root)]);
        }
        for entry in path_entries {
            args.extend(["--path", path_arg(entry)]);
        }
        for variable in &variables {
            args.extend(["--env", variable.as_str()]);
        }
        args.extend(["--revision", &revision]);
        self.cli(&args)
    }

    /// Run a command with the given profile and options.
    pub(crate) fn run(&self, profile: &str, options: &[&str], command: &[&str]) -> Output {
        self.run_for(profile, options, command, super::CLI_DEADLINE)
    }

    /// Run a command with a longer CLI deadline, for real builds and
    /// I/O-heavy cases.
    pub(crate) fn run_for(
        &self,
        profile: &str,
        options: &[&str],
        command: &[&str],
        deadline: Duration,
    ) -> Output {
        let mut args = vec!["run", self.id.as_str(), "--profile", profile];
        args.extend(options);
        args.push("--");
        args.extend(command);
        let result =
            run_with_deadline(run_command(&self.data, &args), deadline).expect("run OMGCode CLI");
        assert!(
            !result.timed_out,
            "run exceeded its {deadline:?} deadline: stderr={}",
            String::from_utf8_lossy(&result.output.stderr)
        );
        result.output
    }

    pub(crate) fn audit(&self) -> Value {
        let output = self.cli(&["runs", "audit", "--limit", "100"]);
        assert_success(&output);
        json_output(&output)
    }

    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.temporary.path().join(name)
    }

    pub(crate) fn command(&self, options: &[&str], command: &[&str]) -> Command {
        let mut args = vec!["run", self.id.as_str(), "--profile", "system"];
        args.extend(options);
        args.push("--");
        args.extend(command);
        let mut process = run_command(&self.data, &args);
        process
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        process
    }
}

/// A CLI `run` command as an operator starts it. On Linux, tree limits need
/// a delegated cgroup, so the CLI starts in a scope of its own through
/// `systemd-run --user --scope -p Delegate=yes`. In scope mode `systemd-run`
/// replaces itself with the CLI, so the child's process ID, signals, standard
/// streams, and exit status are the CLI's own.
pub(crate) fn run_command(data_root: &Path, args: &[&str]) -> Command {
    #[cfg(target_os = "linux")]
    {
        let mut command = Command::new("systemd-run");
        command
            .args(["--user", "--scope", "--quiet", "-p", "Delegate=yes", "--"])
            .arg(env!("CARGO_BIN_EXE_omgcode"))
            .env_remove("OMGCODE_LM_TOKEN")
            .env_remove(super::HANG_FIXTURE_ENV)
            .env(super::HOST_ENV_SENTINEL, super::HOST_ENV_SENTINEL_VALUE)
            .arg("--data-dir")
            .arg(data_root)
            .args(args);
        command
    }
    #[cfg(not(target_os = "linux"))]
    {
        cli_command(data_root, args)
    }
}

impl Drop for Scene {
    fn drop(&mut self) {
        // Fallback for a test that failed before `close`; it must not panic.
        for failure in self.revoke_grants() {
            eprintln!("grant revocation failed during cleanup: {failure}");
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn system_root() -> PathBuf {
    PathBuf::from("/usr")
}

#[cfg(windows)]
pub(crate) fn system_root() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot")).join("System32")
}

/// The profile PATH entry inside the system root.
pub(crate) fn system_path() -> PathBuf {
    if cfg!(windows) {
        system_root()
    } else {
        system_root().join("bin")
    }
}

/// A shell command line for the platform's system shell.
#[cfg(target_os = "linux")]
pub(crate) fn shell(script: &str) -> Vec<&str> {
    vec!["sh", "-c", script]
}

#[cfg(windows)]
pub(crate) fn shell(script: &str) -> Vec<&str> {
    vec!["cmd", "/d", "/c", script]
}

pub(crate) fn run_json(output: &Output) -> Value {
    assert_success(output);
    json_output(output)["run"].clone()
}

pub(crate) fn error_message(output: &Output) -> String {
    let error: Value = serde_json::from_slice(&output.stderr).expect("CLI error is JSON");
    error["error"]["message"]
        .as_str()
        .expect("message")
        .to_owned()
}

/// Wait for a `run` CLI process. On timeout, stop its run and reap the CLI
/// before failing, so no task outlives the test.
pub(crate) fn wait(mut child: std::process::Child, limit: Duration) -> Output {
    let until = Instant::now() + limit;
    loop {
        if child.try_wait().expect("poll child").is_some() {
            return child.wait_with_output().expect("collect output");
        }
        if Instant::now() >= until {
            stop_run(&mut child);
            panic!("the run did not finish within {limit:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// End a `run` CLI and its supervised tree, then reap the CLI.
///
/// On Linux the tree is in its own process group, so killing the CLI alone
/// would leave it running: SIGTERM makes the CLI cancel the tree and record
/// the outcome first. On Windows, killing the CLI closes its job handle,
/// which ends the whole tree.
fn stop_run(child: &mut std::process::Child) {
    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status();
        let grace = Instant::now() + Duration::from_secs(30);
        while Instant::now() < grace {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}
