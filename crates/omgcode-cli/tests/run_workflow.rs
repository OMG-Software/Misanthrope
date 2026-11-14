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
    CLI_DEADLINE, HOST_ENV_SENTINEL, HOST_ENV_SENTINEL_VALUE, assert_control_error,
    assert_error_code, assert_success, cli_command, json_output, path_arg, run_with_deadline,
};

/// Only the journal header: nothing was recorded for refused runs.
fn assert_no_runs(scene: &Scene) {
    let audit = scene.audit();
    assert_eq!(audit["entries"].as_array().expect("entries").len(), 1);
    assert_eq!(audit["pending"], Value::Array(Vec::new()));
}

/// A configured cache root is exposed read-only on Linux without using
/// application storage.
#[cfg(target_os = "linux")]
#[test]
fn a_linux_run_reads_a_configured_package_cache_root() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    let cache_root = scene.path("package-cache-root");
    fs::create_dir(&cache_root).expect("cache root");
    let output = scene.cli(&[
        "workspace",
        "set-package-cache",
        &scene.id,
        path_arg(&cache_root),
        "--revision",
        &scene.revision().to_string(),
    ]);
    assert_success(&output);
    fs::write(cache_root.join("pkg.bin"), b"configured-cache").expect("cache file");
    let cache = cache_root.clone();
    let script = "printf '%s\\n' \"$OMGCODE_PACKAGE_CACHE\"; \
        cat \"$OMGCODE_PACKAGE_CACHE/pkg.bin\" > seen.txt; \
        echo x > \"$OMGCODE_PACKAGE_CACHE/out\" 2>/dev/null && echo writable || echo read-only; \
        ls \"$OMGCODE_PACKAGE_CACHE/..\" >/dev/null 2>&1 && echo parent-listed || echo parent-denied";
    let run = run_json(&scene.run("system", &[], &shell(script)));
    let stdout = run["stdout"]["text"].as_str().expect("stdout");
    assert!(
        stdout.contains(&cache.display().to_string()),
        "the configured cache path was not exposed: {run}"
    );
    assert!(stdout.contains("read-only"), "{run}");
    assert!(stdout.contains("parent-denied"), "{run}");
    assert_eq!(
        fs::read_to_string(scene.workspace.join("seen.txt")).expect("read cache"),
        "configured-cache"
    );
    assert_eq!(run["start"]["package_cache"], true);
    scene.close();
}

/// On Windows a configured cache root is exposed only after the container
/// receives a read-execute grant on the per-workspace cache directory.
#[cfg(windows)]
#[test]
fn a_windows_run_reads_a_granted_package_cache_root() {
    let mut scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    // A top-level root has ancestors app packages can traverse; a root beneath
    // the user's private temp directory cannot be reached by the container.
    let cache_root = tempfile::TempDir::new_in("C:\\").expect("top-level cache root");
    let cache_root_path = cache_root.path().to_path_buf();
    let output = scene.cli(&[
        "workspace",
        "set-package-cache",
        &scene.id,
        path_arg(&cache_root_path),
        "--revision",
        &scene.revision().to_string(),
    ]);
    assert_success(&output);
    fs::write(cache_root_path.join("pkg.bin"), b"configured-cache").expect("cache file");
    let cache = cache_root_path.clone();
    scene.grant(&cache_root_path, "read-execute");
    let script = "type %OMGCODE_PACKAGE_CACHE%\\pkg.bin & echo %OMGCODE_PACKAGE_CACHE%";
    let run = run_json(&scene.run("system", &[], &shell(script)));
    let stdout = run["stdout"]["text"].as_str().expect("stdout");
    assert!(
        stdout.contains(&cache.display().to_string()),
        "the configured cache path was not exposed: {run}"
    );
    assert!(
        stdout.contains("configured-cache"),
        "the cache file was not read: {run}"
    );
    assert_eq!(run["start"]["package_cache"], true);
    scene.close();
}

/// Without the container grant, the configured cache path fails the run
/// before the program starts.
#[cfg(windows)]
#[test]
fn an_ungranted_package_cache_root_fails_the_run() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    let cache_root = scene.path("package-cache-root");
    fs::create_dir(&cache_root).expect("cache root");
    let output = scene.cli(&[
        "workspace",
        "set-package-cache",
        &scene.id,
        path_arg(&cache_root),
        "--revision",
        &scene.revision().to_string(),
    ]);
    assert_success(&output);
    fs::write(cache_root.join("pkg.bin"), b"configured-cache").expect("cache file");
    let output = scene.run("system", &[], &shell("echo should-not-run"));
    assert_error_code(&output, "run");
    scene.close();
}

/// A Linux run can read only its workspace's package cache. The cache is
/// read-only and its parent application directory stays invisible.
#[cfg(target_os = "linux")]
#[test]
fn a_linux_run_reads_only_its_package_cache() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    let cache = scene.data.join("package-cache").join(&scene.id);
    fs::create_dir_all(&cache).expect("cache directory");
    fs::write(cache.join("pkg.bin"), b"cached-package").expect("cache file");
    let script = "printf '%s\\n' \"$OMGCODE_PACKAGE_CACHE\"; \
        cat \"$OMGCODE_PACKAGE_CACHE/pkg.bin\" > seen.txt; \
        echo x > \"$OMGCODE_PACKAGE_CACHE/out\" 2>/dev/null && echo writable || echo read-only; \
        ls \"$OMGCODE_PACKAGE_CACHE/..\" >/dev/null 2>&1 && echo parent-listed || echo parent-denied";
    let run = run_json(&scene.run("system", &[], &shell(script)));
    let stdout = run["stdout"]["text"].as_str().expect("stdout");
    assert!(
        stdout.contains(&cache.display().to_string()),
        "the cache path was not exposed: {run}"
    );
    assert!(stdout.contains("read-only"), "{run}");
    assert!(stdout.contains("parent-denied"), "{run}");
    assert_eq!(
        fs::read_to_string(scene.workspace.join("seen.txt")).expect("read cache"),
        "cached-package"
    );
    assert_eq!(run["start"]["package_cache"], true);
    scene.close();
}

/// Windows does not expose the application-storage cache to the AppContainer.
#[cfg(windows)]
#[test]
fn windows_runs_do_not_see_the_package_cache() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    let cache = scene.data.join("package-cache").join(&scene.id);
    fs::create_dir_all(&cache).expect("cache directory");
    fs::write(cache.join("pkg.bin"), b"cached-package").expect("cache file");
    let run = run_json(&scene.run("system", &[], &shell("set")));
    let stdout = run["stdout"]["text"].as_str().expect("stdout");
    assert!(!stdout.contains("OMGCODE_PACKAGE_CACHE"), "{run}");
    assert!(run["start"].get("package_cache").is_none(), "{run}");
    scene.close();
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
    scene.close();
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
    for flag in ["--read", "--write"] {
        scene.set_permission(flag, "false");
        let output = scene.run("system", &[], &echo);
        assert!(error_message(&output).contains("read and write preferences"));
        scene.set_permission(flag, "true");
    }

    let output = scene.run("missing", &[], &echo);
    assert!(error_message(&output).contains("not recorded"));
    let output = scene.run("system", &["--host-services"], &echo);
    assert!(error_message(&output).contains(if cfg!(windows) {
        "not available on Windows"
    } else {
        "host_services preference is disabled"
    }));
    let output = scene.run("system", &["--network"], &echo);
    assert!(error_message(&output).contains("task_network preference is disabled"));
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
    scene.close();
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
    scene.close();
}

/// Network access needs the `task_network` preference and the per-run flag;
/// the start record lists the host files it exposed. The preference alone
/// changes nothing. On Linux the resolver files are readable only with it.
#[test]
fn network_needs_the_preference_and_the_flag() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    scene.set_permission("--task-network", "true");
    #[cfg(target_os = "linux")]
    let probe = shell("cat /etc/hosts > /dev/null && echo hosts-readable");
    #[cfg(windows)]
    let probe = shell("echo networked");

    let plain = run_json(&scene.run("system", &[], &probe));
    assert!(plain["start"].get("network").is_none(), "{plain}");
    let networked = run_json(&scene.run("system", &["--network"], &probe));
    let support = networked["start"]["network"]["support_paths"]
        .as_array()
        .expect("support paths");
    #[cfg(target_os = "linux")]
    {
        let readable = |run: &Value| {
            run["stdout"]["text"]
                .as_str()
                .expect("stdout")
                .contains("hosts-readable")
        };
        assert!(!readable(&plain), "{plain}");
        assert!(readable(&networked), "{networked}");
        assert!(!support.is_empty());
        assert_eq!(networked["finish"]["confinement"]["kind"], "landlock");
    }
    #[cfg(windows)]
    {
        assert!(support.is_empty(), "{networked}");
        assert_eq!(networked["finish"]["confinement"]["kind"], "app_container");
    }
    assert_eq!(networked["finish"]["result"]["kind"], "exited");
    scene.close();
}

/// Live check that a networked run resolves a name and completes a TLS
/// handshake with the system `curl`, and that the same run fails without
/// `--network`. It needs internet access, so it runs only when asked:
/// `OMGCODE_TEST_INTERNET_URL=https://example.com/ cargo test -- --ignored`.
#[test]
#[ignore = "needs internet access, curl, and OMGCODE_TEST_INTERNET_URL"]
fn a_networked_run_fetches_over_tls() {
    let url = std::env::var("OMGCODE_TEST_INTERNET_URL").expect("OMGCODE_TEST_INTERNET_URL");
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    scene.set_permission("--task-network", "true");
    let curl = system_root().join(if cfg!(windows) {
        "curl.exe"
    } else {
        "bin/curl"
    });
    let command = [
        path_arg(&curl),
        "-sS",
        "--max-time",
        "20",
        "-o",
        "fetched.html",
        "-w",
        "%{http_code}",
        url.as_str(),
    ];
    let denied = run_json(&scene.run("system", &[], &command));
    assert_ne!(denied["finish"]["result"]["code"], 0, "{denied}");
    let fetched = run_json(&scene.run("system", &["--network"], &command));
    assert_eq!(fetched["finish"]["result"]["kind"], "exited", "{fetched}");
    assert_eq!(fetched["finish"]["result"]["code"], 0, "{fetched}");
    let status = fetched["stdout"]["text"].as_str().expect("stdout");
    assert!(
        status.starts_with('2') || status.starts_with('3'),
        "{status}"
    );
    assert!(scene.workspace.join("fetched.html").exists());
    scene.close();
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
    scene.close();
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
    scene.close();
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
    scene.close();
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
    scene.close();
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
    scene.close();
}

/// A run whose caller dies stays pending; archiving waits until the
/// operator acknowledges it.
#[test]
fn an_abandoned_run_is_pending_until_acknowledged() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    let mut child = scene
        .command(&[], &shell(LONG_RUN))
        .spawn()
        .expect("spawn run");
    let run_id = wait_for_start(&scene);
    child.kill().expect("kill the CLI");
    child.wait().expect("reap the CLI");
    // On Linux, killing the CLI leaves the run's process group behind;
    // on Windows, closing the CLI's job handle already ended the tree.
    #[cfg(target_os = "linux")]
    stop_group(&scene);
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
    // A bare file name names a file in the current directory.
    let name = fs::read_dir(&archive)
        .expect("archive listing")
        .next()
        .expect("archived journal")
        .expect("archive entry")
        .file_name();
    let name = name.to_str().expect("archive name");
    let mut command = cli_command(&scene.data, &["runs", "audit", "--archive", name]);
    command.current_dir(&archive);
    let output = run_with_deadline(command, CLI_DEADLINE).expect("run the CLI");
    assert!(!output.timed_out);
    assert_success(&output.output);
    assert_eq!(
        json_output(&output.output)["entries"]
            .as_array()
            .expect("archived entries")
            .len(),
        3
    );
    scene.close();
}

/// An interrupt cancels the run, terminates the tree, and is recorded.
#[cfg(target_os = "linux")]
#[test]
fn an_interrupt_cancels_and_records_the_run() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    let child = scene
        .command(&[], &shell(LONG_RUN))
        .spawn()
        .expect("spawn run");
    wait_for_start(&scene);
    let status = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("send SIGINT");
    assert!(status.success());
    let run = run_json(&wait(child, Duration::from_secs(20)));
    assert_eq!(run["finish"]["result"]["kind"], "cancelled");
    assert_eq!(scene.audit()["pending"], Value::Array(Vec::new()));
    scene.close();
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
    scene.close();
}

/// A run that writes `started.txt` once its tree is running, then runs until
/// it is stopped. On Linux the marker holds the shell's process ID, which is
/// also the run's process group, because the runner makes it the leader.
#[cfg(target_os = "linux")]
const LONG_RUN: &str = "sleep 30 & echo $$ > started.txt; wait";
#[cfg(windows)]
const LONG_RUN: &str = "echo started>started.txt& for /l %i in (1,0,2) do @rem";

/// Wait until the run's program wrote its start marker, and return the run
/// identifier. The marker proves the tree is running, not only recorded.
fn wait_for_start(scene: &Scene) -> String {
    let marker = scene.workspace.join("started.txt");
    let until = Instant::now() + Duration::from_secs(20);
    while !marker.exists() {
        assert!(Instant::now() < until, "the run never started");
        thread::sleep(Duration::from_millis(50));
    }
    let audit = scene.audit();
    audit["pending"][0]["run_id"]
        .as_str()
        .expect("the started run is pending")
        .to_owned()
}

/// Kill the process group of a run whose CLI was killed, and wait until no
/// member remains, so the test never leaves a detached tree behind.
#[cfg(target_os = "linux")]
fn stop_group(scene: &Scene) {
    let marker = fs::read_to_string(scene.workspace.join("started.txt")).expect("start marker");
    let group = format!("-{}", marker.trim().parse::<u32>().expect("process group"));
    // The group may already be gone; the probe below decides.
    let _ = Command::new("kill")
        .args(["-s", "KILL", "--", &group])
        .status();
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let alive = Command::new("kill")
            .args(["-0", "--", &group])
            .stderr(std::process::Stdio::null())
            .status()
            .expect("probe the process group")
            .success();
        if !alive {
            return;
        }
        assert!(Instant::now() < until, "the run's process group remained");
        thread::sleep(Duration::from_millis(20));
    }
}

/// The newest journal entry's recorded result.
fn last_result(scene: &Scene) -> Value {
    let audit = scene.audit();
    let entries = audit["entries"].as_array().expect("entries");
    entries.last().expect("an entry")["action"]["payload"]["finish"]["result"].clone()
}

/// Narrowing a preference stops a live run before the settings command
/// reports success, and a live run cannot be acknowledged meanwhile.
#[test]
fn narrowing_settings_stops_a_live_run_before_reporting() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    let child = scene
        .command(&[], &shell(LONG_RUN))
        .spawn()
        .expect("spawn run");
    let run_id = wait_for_start(&scene);
    let output = scene.cli(&["runs", "acknowledge", &run_id]);
    assert!(error_message(&output).contains("still live"));
    // A change that withdraws nothing leaves the run going.
    scene.set_permission("--model-tools", "true");
    assert_eq!(scene.audit()["pending"][0]["run_id"], run_id.as_str());
    scene.set_permission("--execute", "false");
    // The command returned only after the outcome was recorded.
    assert_eq!(scene.audit()["pending"], Value::Array(Vec::new()));
    assert_eq!(last_result(&scene)["kind"], "revoked");
    let run = run_json(&wait(child, Duration::from_secs(30)));
    assert_eq!(run["finish"]["result"]["kind"], "revoked");
    assert!(
        run["revoked_because"]
            .as_str()
            .expect("reason")
            .contains("execute")
    );
    scene.close();
}

/// Removing the run's profile stops the run before the command reports.
#[test]
fn removing_the_profile_stops_a_live_run() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    let child = scene
        .command(&[], &shell(LONG_RUN))
        .spawn()
        .expect("spawn run");
    wait_for_start(&scene);
    let revision = scene.revision().to_string();
    assert_success(&scene.settings_cli(&[
        "workspace",
        "profile-remove",
        "system",
        "--revision",
        &revision,
    ]));
    assert_eq!(scene.audit()["pending"], Value::Array(Vec::new()));
    let run = run_json(&wait(child, Duration::from_secs(30)));
    assert_eq!(run["finish"]["result"]["kind"], "revoked");
    assert!(
        run["revoked_because"]
            .as_str()
            .expect("reason")
            .contains("profile")
    );
    scene.close();
}

/// An interrupted append can leave the run journal unreadable. Revoking a
/// container grant still removes the grant, then reports the failure.
#[cfg(windows)]
#[test]
fn an_unreadable_run_journal_does_not_keep_a_container_grant() {
    let mut scene = Scene::new();
    let tools = scene.path("tools");
    fs::create_dir(&tools).expect("tools directory");
    scene.grant(&tools, "read-execute");
    let grant = scene.grants.last().expect("read grant").clone();
    let journal = scene.data.join("runs-v1.jsonl");
    fs::write(&journal, b"{\"sequence\":").expect("partial record");
    let revoke = scene.settings_cli(&["workspace", "container-revoke", &grant]);
    assert!(!revoke.status.success(), "revocation reported success");
    assert!(
        String::from_utf8_lossy(&revoke.stderr).contains("was revoked"),
        "{}",
        String::from_utf8_lossy(&revoke.stderr)
    );
    // Remove the partial record so the scene can revoke its other grants.
    fs::remove_file(&journal).expect("remove partial record");
    let status = scene.cli(&["workspace", "status"]);
    assert_success(&status);
    assert!(
        !String::from_utf8_lossy(&status.stdout).contains(&grant),
        "the revoked grant is still listed"
    );
    scene.grants.retain(|tracked| tracked != &grant);
    scene.close();
}

/// Revoking any container grant of the workspace, not only its read-write
/// grant, stops a live run before the command reports. The run never needed
/// this read-execute grant, but its container held it while it ran.
#[cfg(windows)]
#[test]
fn revoking_a_read_grant_stops_a_live_run() {
    let mut scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    let tools = scene.path("tools");
    fs::create_dir(&tools).expect("tools directory");
    scene.grant(&tools, "read-execute");
    // The scene keeps the grant until the revocation succeeds, so a failure
    // before then still revokes it when the scene is dropped.
    let grant = scene.grants.last().expect("read grant").clone();
    let child = scene
        .command(&[], &shell(LONG_RUN))
        .spawn()
        .expect("spawn run");
    wait_for_start(&scene);
    assert_success(&scene.settings_cli(&["workspace", "container-revoke", &grant]));
    scene.grants.retain(|tracked| tracked != &grant);
    // The command returned only after the outcome was recorded.
    assert_eq!(scene.audit()["pending"], Value::Array(Vec::new()));
    let run = run_json(&wait(child, Duration::from_secs(30)));
    assert_eq!(run["finish"]["result"]["kind"], "revoked");
    assert!(
        run["revoked_because"]
            .as_str()
            .expect("reason")
            .contains("revoked")
    );
    scene.close();
}

/// A busy loop that runs until it is stopped.
#[cfg(target_os = "linux")]
const SPIN: &str = "while :; do :; done";
#[cfg(windows)]
const SPIN: &str = "for /l %i in (1,0,2) do @rem";

/// Every run records its tree limits. Reaching one ends the whole run, which
/// records the limit and what the tree used.
#[test]
fn a_tree_limit_ends_the_run_and_is_recorded() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    let options = [
        "--max-cpu-secs",
        "1",
        "--max-memory-mib",
        "512",
        "--max-processes",
        "64",
        "--max-disk-mib",
        "256",
    ];
    let run = run_json(&scene.run("system", &options, &shell(SPIN)));
    assert_eq!(run["start"]["limits"]["max_cpu_ms"], 1000);
    assert_eq!(
        run["start"]["limits"]["max_memory_bytes"],
        512u64 * 1024 * 1024
    );
    assert_eq!(run["start"]["limits"]["max_processes"], 64);
    assert_eq!(
        run["start"]["limits"]["max_disk_bytes"],
        256u64 * 1024 * 1024
    );
    assert_eq!(run["finish"]["result"]["kind"], "limit_exceeded");
    assert_eq!(run["finish"]["result"]["limit"], "cpu_time");
    assert!(run["finish"]["usage"]["cpu_ms"].as_u64().expect("CPU time") >= 1000);
    assert_eq!(last_result(&scene)["limit"], "cpu_time");
    assert_eq!(scene.audit()["pending"], Value::Array(Vec::new()));
    // Without flags, the default limits apply and are recorded.
    let run = run_json(&scene.run("system", &[], &shell("exit 0")));
    assert_eq!(run["finish"]["result"]["code"], 0);
    assert_eq!(run["start"]["limits"]["max_processes"], 1024);
    assert_eq!(run["start"]["limits"]["max_cpu_ms"], 7_200_000);
    assert_eq!(
        run["start"]["limits"]["max_disk_bytes"],
        8u64 * 1024 * 1024 * 1024
    );
    assert!(run["finish"]["usage"].is_object());
    scene.close();
}

/// Growth in the writable workspace past the disk limit ends the whole run.
/// The limit is recorded before the program starts, and the outcome reports
/// the measured peak growth.
#[test]
fn a_disk_limit_ends_the_run_and_is_recorded() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    // The source exists before the run, so the baseline counts it. Growing the
    // writable workspace adds bytes the disk limit must see; each command then
    // waits to be stopped.
    #[cfg(target_os = "linux")]
    let source = scene.workspace.join("growth-source.bin");
    #[cfg(target_os = "linux")]
    fs::write(&source, vec![0u8; 32 * 1024 * 1024]).expect("growth source");
    #[cfg(target_os = "linux")]
    let script = "cp growth-source.bin growth.bin; while :; do :; done".to_owned();
    // Build the growth with `cmd` built-ins and files the container creates.
    // External tools and `copy` were denied in the CI container, but `echo`
    // creation and `type` of a container-created file work. Append a 7 KiB
    // chunk enough times to pass 16 MiB.
    #[cfg(windows)]
    let script = {
        let chunk = "x".repeat(7000);
        format!(
            "echo {chunk}>chunk & (for /l %i in (1,1,2500) do @type chunk>>growth.bin) & for /l %i in (1,0,2) do @rem"
        )
    };
    let options = [
        "--max-disk-mib",
        "16",
        "--max-memory-mib",
        "512",
        // Creating 32 MiB and sampling once a second can outlast the normal
        // 10-second CLI deadline on a loaded runner. Bound the run itself so
        // a missed disk limit still records a deadline instead of leaving the
        // run pending when the harness stops the CLI.
        "--timeout-secs",
        "45",
    ];
    let run =
        run_json(&scene.run_for("system", &options, &shell(&script), Duration::from_secs(60)));
    assert_eq!(
        run["start"]["limits"]["max_disk_bytes"],
        16u64 * 1024 * 1024,
        "{run}"
    );
    assert_eq!(run["finish"]["result"]["kind"], "limit_exceeded", "{run}");
    assert_eq!(run["finish"]["result"]["limit"], "disk", "{run}");
    assert!(
        run["finish"]["usage"]["peak_disk_bytes"]
            .as_u64()
            .expect("disk usage")
            >= 16 * 1024 * 1024,
        "{run}"
    );
    assert_eq!(last_result(&scene)["limit"], "disk");
    assert_eq!(scene.audit()["pending"], Value::Array(Vec::new()));
    scene.close();
}

/// Out-of-range tree limits are refused before anything is recorded.
#[test]
fn out_of_range_tree_limits_are_refused_before_recording() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    for options in [
        ["--max-memory-mib", "1"],
        ["--max-memory-mib", "1048577"],
        ["--max-processes", "0"],
        ["--max-processes", "65537"],
        ["--max-cpu-secs", "0"],
        ["--max-cpu-secs", "604801"],
        ["--max-disk-mib", "15"],
        ["--max-disk-mib", "1048577"],
    ] {
        let output = scene.run("system", &options, &shell("exit 0"));
        assert_error_code(&output, "configuration");
    }
    assert_no_runs(&scene);
    scene.close();
}

/// On Linux, a run needs a delegated cgroup for its limits. A CLI started
/// outside one is refused before anything is recorded.
#[cfg(target_os = "linux")]
#[test]
fn a_run_outside_a_delegated_cgroup_is_refused_before_recording() {
    let scene = Scene::new();
    assert_success(&scene.add_profile("system", &[]));
    let output = scene.cli(&[
        "run",
        &scene.id,
        "--profile",
        "system",
        "--",
        "sh",
        "-c",
        "exit 0",
    ]);
    assert_error_code(&output, "run");
    assert!(error_message(&output).contains("tree limits"));
    assert_no_runs(&scene);
    scene.close();
}
