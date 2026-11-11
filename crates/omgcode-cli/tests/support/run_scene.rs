//! A registered workspace scene for `run` workflow and acceptance tests.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

use super::{assert_success, json_output, path_arg, run_cli, run_cli_for};

/// CLI deadline for a container grant or revocation.
const GRANT_DEADLINE: Duration = Duration::from_secs(300);

/// A development root with one registered workspace whose `execute`
/// preference is on, and, on Windows, a read-write grant on the workspace.
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
        assert_success(&output);
        self.grants.push(
            json_output(&output)["grant_id"]
                .as_str()
                .expect("grant id")
                .to_owned(),
        );
    }

    pub(crate) fn revision(&self) -> u64 {
        let output = self.cli(&["workspace", "status"]);
        assert_success(&output);
        json_output(&output)["revision"].as_u64().expect("revision")
    }

    pub(crate) fn set_permission(&self, flag: &str, value: &str) {
        assert_success(&self.cli(&[
            "workspace",
            "permissions",
            &self.id,
            flag,
            value,
            "--revision",
            &self.revision().to_string(),
        ]));
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

    /// Run a command with a longer CLI deadline, for real builds.
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
        run_cli_for(&self.data, &args, deadline)
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
        let mut process = Command::new(env!("CARGO_BIN_EXE_omgcode"));
        process
            .env_remove("OMGCODE_LM_TOKEN")
            .arg("--data-dir")
            .arg(&self.data)
            .args(["run", &self.id, "--profile", "system"])
            .args(options)
            .arg("--")
            .args(command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        process
    }
}

impl Drop for Scene {
    fn drop(&mut self) {
        // Revoking the last grant also deletes the container profile, so
        // test runs leave no per-user AppContainer state behind.
        for grant in &self.grants {
            let _ = run_cli_for(
                &self.data,
                &["workspace", "container-revoke", grant],
                GRANT_DEADLINE,
            );
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

pub(crate) fn wait(mut child: std::process::Child, limit: Duration) -> Output {
    let until = Instant::now() + limit;
    loop {
        if child.try_wait().expect("poll child").is_some() {
            return child.wait_with_output().expect("collect output");
        }
        if Instant::now() >= until {
            let _ = child.kill();
            panic!("the run did not finish within {limit:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}
