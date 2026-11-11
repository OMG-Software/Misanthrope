//! Trusted `run` workflows through the real CLI, with system tools only.
//!
//! Linux runs `/usr/bin/sh` under Landlock; Windows runs `cmd.exe` in the
//! workspace AppContainer, so each Windows scene grants and later revokes
//! the workspace directory. Toolchain acceptance cases are in
//! `toolchain_acceptance.rs`.
#![cfg(any(target_os = "linux", windows))]

mod support;

use serde_json::Value;
use std::fs;
#[cfg(target_os = "linux")]
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use support::run_scene::{Scene, error_message, run_json, shell, system_root, wait};
use support::{
    HOST_ENV_SENTINEL, HOST_ENV_SENTINEL_VALUE, assert_control_error, assert_error_code,
    assert_success, json_output, path_arg,
};

/// Only the journal header: nothing was recorded for refused runs.
fn assert_no_runs(scene: &Scene) {
    let audit = scene.audit();
    assert_eq!(audit["entries"].as_array().expect("entries").len(), 1);
    assert_eq!(audit["pending"], Value::Array(Vec::new()));
}

/// A run writes in the workspace, cannot read host data, sees only the
/// curated environment, and records its intent and outcome but not its
/// argument text or output.
#[test]
fn a_run_is_confined_and_recorded_without_its_arguments() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    let secret = scene.path("host-secret.txt");
    fs::write(&secret, "host-data-sentinel").expect("host file");
    let sentinel = "argument-sentinel-6f1d";
    #[cfg(target_os = "linux")]
    let script = format!(
        "echo {sentinel} > out.txt; cat out.txt; cat '{}'; env",
        secret.display()
    );
    #[cfg(windows)]
    let script = format!(
        "echo {sentinel}>out.txt& type out.txt& type \"{}\"& set",
        secret.display()
    );
    let run = run_json(&scene.run("system", &[], &shell(&script)));
    let stdout = run["stdout"]["text"].as_str().expect("stdout");
    assert!(stdout.contains(sentinel), "{stdout}");
    assert!(!stdout.contains("host-data-sentinel"), "{stdout}");
    assert!(
        fs::read_to_string(scene.workspace.join("out.txt"))
            .expect("workspace output")
            .contains(sentinel)
    );
    assert!(stdout.contains("PATH="), "{stdout}");
    assert!(!stdout.contains("OMGCODE_LM_TOKEN"), "{stdout}");
    // The CLI process has this variable; the run must not inherit it.
    assert!(!stdout.contains(HOST_ENV_SENTINEL), "{stdout}");
    assert!(!stdout.contains(HOST_ENV_SENTINEL_VALUE), "{stdout}");
    assert_eq!(run["finish"]["result"]["kind"], "exited");
    assert_eq!(run["start"]["profile_name"], "system");
    assert_eq!(run["start"]["unix_sockets"], "pairs_only");
    assert_eq!(run["start"]["argument_count"], shell("").len() - 1);
    #[cfg(target_os = "linux")]
    {
        assert_eq!(run["finish"]["confinement"]["kind"], "landlock");
        assert!(stdout.contains("HOME=") && stdout.contains("run-scratch"));
        assert_eq!(run["scratch_removed"], true);
    }
    #[cfg(windows)]
    {
        assert_eq!(run["finish"]["confinement"]["kind"], "app_container");
        assert!(stdout.contains("USERPROFILE=") && stdout.contains("Packages"));
    }

    let audit = scene.audit();
    let entries = audit["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[1]["action"]["type"], "run_started");
    assert_eq!(entries[2]["action"]["type"], "run_finished");
    let journal = serde_json::to_string(&audit).expect("audit text");
    assert!(!journal.contains(sentinel));
    assert!(!journal.contains("host-data-sentinel"));
    assert_eq!(audit["pending"], Value::Array(Vec::new()));
}

/// Every refusal happens before anything is recorded or started.
#[test]
fn refused_runs_record_nothing() {
    let scene = Scene::build(false, true);
    assert_success(&scene.add_profile("system", &[]));
    let echo = shell("echo refused");

    let output = scene.run("system", &[], &echo);
    assert_error_code(&output, "run");
    assert!(error_message(&output).contains("execute preference is disabled"));
    scene.set_permission("--execute", "true");

    let output = scene.run("missing", &[], &echo);
    assert!(error_message(&output).contains("not recorded"));
    let output = scene.run("system", &["--host-services"], &echo);
    assert!(error_message(&output).contains(if cfg!(windows) {
        "not available on Windows"
    } else {
        "host_services preference is disabled"
    }));
    let output = scene.run("system", &[], &["no-such-program-x1"]);
    assert!(error_message(&output).contains("no profile PATH entry"));

    let outside = scene.path("outside");
    fs::create_dir(&outside).expect("outside directory");
    let stray = outside.join(if cfg!(windows) { "stray.exe" } else { "stray" });
    fs::write(&stray, b"not a program").expect("stray file");
    let output = scene.run("system", &[], &[path_arg(&stray)]);
    assert!(error_message(&output).contains("inside a profile root or the workspace"));
    let output = scene.run("system", &[], &["../escape/x"]);
    assert!(error_message(&output).contains("stay inside the workspace"));
    let output = scene.run("system", &["--cwd", ".."], &echo);
    assert!(error_message(&output).contains("workspace-relative"));
    let output = scene.run("system", &["--cwd", "missing"], &echo);
    assert_error_code(&output, "run");
    let output = scene.run("system", &["--timeout-secs", "0"], &echo);
    assert_error_code(&output, "configuration");
    let output = scene.run("system", &["--timeout-secs", "3601"], &echo);
    assert_error_code(&output, "configuration");
    assert_no_runs(&scene);
}

/// Host services need the preference and the per-run flag; the run record
/// notes the mode. Windows refuses it even with the preference.
#[test]
fn host_services_needs_the_preference_and_the_flag() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    scene.set_permission("--host-services", "true");
    let output = scene.run("system", &["--host-services"], &shell("echo services"));
    if cfg!(windows) {
        assert!(error_message(&output).contains("not available on Windows"));
        assert_no_runs(&scene);
    } else {
        let run = run_json(&output);
        assert_eq!(run["start"]["unix_sockets"], "host_services");
        assert_eq!(run["finish"]["confinement"]["kind"], "landlock");
    }
}

/// Windows needs a recorded read-write grant on the workspace.
#[cfg(windows)]
#[test]
fn windows_runs_need_the_workspace_grant() {
    let scene = Scene::build(true, false);
    assert_success(&scene.add_profile("system", &[]));
    let output = scene.run("system", &[], &shell("echo ungranted"));
    assert!(error_message(&output).contains("grant the workspace directory"));
    assert_no_runs(&scene);
}

/// A moved or replaced profile root is stale and refused; it is usable again
/// once the recorded directory is back.
#[test]
fn stale_profiles_are_refused() {
    let mut scene = Scene::new();
    let tools = scene.path("tools");
    fs::create_dir(&tools).expect("tools");
    if cfg!(windows) {
        scene.grant(&tools, "read-execute");
    }
    assert_success(&scene.add_profile("system", &[&tools]));
    run_json(&scene.run("system", &[], &shell("echo fresh")));
    fs::rename(&tools, scene.path("tools-old")).expect("move tools");
    fs::create_dir(&tools).expect("replacement tools");
    let output = scene.run("system", &[], &shell("echo stale"));
    assert!(
        error_message(&output).contains("stale"),
        "{}",
        error_message(&output)
    );
    assert_eq!(
        scene.audit()["entries"].as_array().expect("entries").len(),
        3
    );
    fs::remove_dir(&tools).expect("remove replacement");
    fs::rename(scene.path("tools-old"), &tools).expect("restore tools");
    run_json(&scene.run("system", &[], &shell("echo restored")));
}

/// The deadline terminates the whole tree and is recorded.
#[test]
fn the_deadline_terminates_and_is_recorded() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    #[cfg(target_os = "linux")]
    let script = "sleep 30 & sleep 30";
    #[cfg(windows)]
    let script = "for /l %i in (1,0,2) do @rem";
    let started = Instant::now();
    let run = run_json(&scene.run("system", &["--timeout-secs", "1"], &shell(script)));
    assert_eq!(run["finish"]["result"]["kind"], "deadline_exceeded");
    assert!(started.elapsed() < Duration::from_secs(9));
}

/// Two runs in the same workspace at once both complete and are recorded.
#[test]
fn concurrent_runs_complete_and_are_recorded() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    #[cfg(target_os = "linux")]
    let scripts = ["sleep 1; echo one > one.txt", "sleep 1; echo two > two.txt"];
    #[cfg(windows)]
    let scripts = [
        "(for /l %i in (1,1,30000) do @cd .)& echo one>one.txt",
        "(for /l %i in (1,1,30000) do @cd .)& echo two>two.txt",
    ];
    let children: Vec<_> = scripts
        .iter()
        .map(|script| {
            scene
                .command(&[], &shell(script))
                .spawn()
                .expect("spawn run")
        })
        .collect();
    for child in children {
        let output = wait(child, Duration::from_secs(60));
        assert_eq!(run_json(&output)["finish"]["result"]["code"], 0);
    }
    assert!(scene.workspace.join("one.txt").exists());
    assert!(scene.workspace.join("two.txt").exists());
    let audit = scene.audit();
    assert_eq!(audit["entries"].as_array().expect("entries").len(), 5);
    assert_eq!(audit["pending"], Value::Array(Vec::new()));
}

/// A program inside the workspace can run.
#[test]
fn workspace_programs_run() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    let bin = scene.workspace.join("bin");
    fs::create_dir(&bin).expect("bin");
    #[cfg(target_os = "linux")]
    let (source, name, command) = (
        "/usr/bin/echo",
        "bin/echo",
        vec!["bin/echo", "from-workspace"],
    );
    // A copied cmd.exe lacks its message resources, so use hostname.exe.
    #[cfg(windows)]
    let (source, name, command) = (
        system_root().join("hostname.exe"),
        "bin/hostname.exe",
        vec!["bin/hostname.exe"],
    );
    fs::copy(source, scene.workspace.join(name)).expect("copy program");
    let run = run_json(&scene.run("system", &[], &command));
    assert_eq!(run["finish"]["result"]["code"], 0);
    let stdout = run["stdout"]["text"].as_str().expect("stdout");
    if cfg!(windows) {
        assert!(!stdout.trim().is_empty());
    } else {
        assert!(stdout.contains("from-workspace"));
    }
}

/// A run whose caller dies stays pending; archiving waits until the
/// operator acknowledges it.
#[test]
fn an_abandoned_run_is_pending_until_acknowledged() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    #[cfg(target_os = "linux")]
    let script = "exec sleep 30";
    #[cfg(windows)]
    let script = "for /l %i in (1,0,2) do @rem";
    let mut child = scene
        .command(&[], &shell(script))
        .spawn()
        .expect("spawn run");
    let run_id = wait_for_pending(&scene);
    child.kill().expect("kill the CLI");
    child.wait().expect("reap the CLI");
    let archive = scene.path("archive");
    fs::create_dir(&archive).expect("archive directory");
    let output = scene.cli(&["runs", "archive", "--archive-dir", path_arg(&archive)]);
    assert!(error_message(&output).contains("pending"));
    assert_success(&scene.cli(&["runs", "acknowledge", &run_id]));
    let output = scene.cli(&["runs", "archive", "--archive-dir", path_arg(&archive)]);
    assert_success(&output);
    let archived = json_output(&output);
    assert_eq!(archived["event_count"], 3);
    assert_eq!(
        scene.audit()["entries"].as_array().expect("entries").len(),
        1
    );
}

/// An interrupt cancels the run, terminates the tree, and is recorded.
#[cfg(target_os = "linux")]
#[test]
fn an_interrupt_cancels_and_records_the_run() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    let child = scene
        .command(&[], &shell("sleep 30 & sleep 30"))
        .spawn()
        .expect("spawn run");
    wait_for_pending(&scene);
    let status = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("send SIGINT");
    assert!(status.success());
    let run = run_json(&wait(child, Duration::from_secs(20)));
    assert_eq!(run["finish"]["result"]["kind"], "cancelled");
    assert_eq!(scene.audit()["pending"], Value::Array(Vec::new()));
}

/// Profiles are added, shown, listed, and removed with revisions.
#[test]
fn profiles_are_managed_with_revisions() {
    let scene = Scene::new();
    let output = scene.cli(&[
        "workspace",
        "profile-add",
        "bad",
        "--root",
        path_arg(&system_root()),
        "--env",
        "NO_EQUALS",
        "--revision",
        &scene.revision().to_string(),
    ]);
    assert_error_code(&output, "configuration");
    assert_success(&scene.add_profile("system", &[]));
    let output = scene.cli(&["workspace", "profile-show", "SYSTEM"]);
    assert_success(&output);
    assert_eq!(json_output(&output)["profile"]["name"], "system");
    let output = scene.cli(&["workspace", "status"]);
    assert_eq!(json_output(&output)["profiles"][0]["name"], "system");
    assert_control_error(&scene.add_profile("system", &[]));
    assert_error_code(
        &scene.cli(&["workspace", "profile-remove", "system", "--revision", "0"]),
        "control",
    );
    let revision = scene.revision().to_string();
    assert_success(&scene.cli(&[
        "workspace",
        "profile-remove",
        "system",
        "--revision",
        &revision,
    ]));
    assert_error_code(
        &scene.cli(&["workspace", "profile-show", "system"]),
        "control",
    );
}

fn wait_for_pending(scene: &Scene) -> String {
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        let audit = scene.audit();
        if let Some(run) = audit["pending"].get(0) {
            // Give the runner a moment to start the program after the intent.
            thread::sleep(Duration::from_millis(500));
            return run["run_id"].as_str().expect("run id").to_owned();
        }
        assert!(Instant::now() < until, "the run never became pending");
        thread::sleep(Duration::from_millis(50));
    }
}
