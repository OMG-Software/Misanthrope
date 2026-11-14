//! Runner tests. The unit-test binary re-executes itself as the child fixture,
//! so the tests depend on no external program or `PATH` entry.

use std::collections::BTreeSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use super::{
    AppliedConfinement, CancelSignal, Confinement, MAX_CAPTURE_BYTES, MAX_CLEANUP_TIMEOUT,
    MAX_CONFINEMENT_PATHS, MAX_DEADLINE, ProcessError, ProcessLimits, ProcessSpec, Termination,
    TreeLimits, run, run_confined,
};

#[cfg(target_os = "linux")]
mod confinement;
mod tree_limits;
#[cfg(windows)]
mod windows_confinement;

const FIXTURE_ENV: &str = "OMGCODE_PROCESS_FIXTURE";
const RECORD_ENV: &str = "OMGCODE_PROCESS_FIXTURE_RECORD";
const GROW_PATH_ENV: &str = "OMGCODE_PROCESS_GROW_PATH";
const FIXTURE_NAME: &str = "process::tests::fixture";
const HARNESS_ARGS: [&str; 5] = [
    "--exact",
    FIXTURE_NAME,
    "--nocapture",
    "--test-threads=1",
    "-q",
];
const FLOOD_BYTES: usize = 1024 * 1024;
/// Bytes the `grow-disk` fixture writes before it waits to be stopped.
const GROW_BYTES: usize = 64 * 1024 * 1024;

/// Child entry point. It does nothing unless the runner selected a mode.
#[test]
fn fixture() {
    let Ok(mode) = env::var(FIXTURE_ENV) else {
        return;
    };
    let code = match mode.as_str() {
        "echo" => {
            print!("out-marker");
            eprint!("err-marker");
            7
        }
        "env" => {
            for (name, _) in env::vars_os() {
                println!("ENV:{}", name.to_string_lossy());
            }
            0
        }
        "args" => {
            for argument in env::args_os().skip(1 + HARNESS_ARGS.len()) {
                println!("ARG:{argument:?}");
            }
            0
        }
        "flood" => {
            let chunk = [b'x'; 64 * 1024];
            let mut stdout = std::io::stdout().lock();
            for _ in 0..FLOOD_BYTES / chunk.len() {
                stdout.write_all(&chunk).expect("write flood");
            }
            0
        }
        "sleep" => {
            thread::sleep(Duration::from_secs(120));
            0
        }
        "ready-sleep" => {
            write_record(&format!("pid={}\n", std::process::id()));
            thread::sleep(Duration::from_secs(120));
            0
        }
        "grandchild" => start_grandchild(),
        "grandchild-sleep" => {
            let escape = attempt_escape();
            write_record(&format!("pid={}\nescape={escape}\n", std::process::id()));
            thread::sleep(Duration::from_secs(120));
            0
        }
        #[cfg(target_os = "linux")]
        "probe" => confinement::probes(),
        #[cfg(target_os = "linux")]
        "probe-grandchild" => confinement::grandchild_probes(),
        #[cfg(windows)]
        "probe" => windows_confinement::probes(),
        #[cfg(windows)]
        "probe-grandchild" => windows_confinement::grandchild_probes(),
        "hog-memory" => tree_limits::hog_memory(),
        "spawn-many" => tree_limits::spawn_many(),
        "spin" => tree_limits::spin(),
        "grow-disk" => grow_disk(),
        #[cfg(windows)]
        "nested-job" => tree_limits::nested_job(),
        #[cfg(target_os = "linux")]
        "signal" => {
            rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::TERM)
                .expect("signal self");
            thread::sleep(Duration::from_secs(120));
            0
        }
        _ => 99,
    };
    std::io::stdout().flush().expect("flush stdout");
    std::io::stderr().flush().expect("flush stderr");
    std::process::exit(code);
}

fn write_record(text: &str) {
    let path = PathBuf::from(env::var_os(RECORD_ENV).expect("record path"));
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, text).expect("write record");
    fs::rename(&temporary, &path).expect("publish record");
}

/// Write allocated bytes to the path named by `GROW_PATH_ENV`, sync them, and
/// wait to be stopped. The sync makes the allocation visible to the sampler.
fn grow_disk() -> i32 {
    let path = PathBuf::from(env::var_os(GROW_PATH_ENV).expect("growth path"));
    let chunk = vec![b'x'; 1024 * 1024];
    let mut file = fs::File::create(&path).expect("create growth file");
    for _ in 0..GROW_BYTES / chunk.len() {
        file.write_all(&chunk).expect("write growth file");
    }
    file.sync_all().expect("sync growth file");
    thread::sleep(Duration::from_secs(120));
    0
}

/// Start a grandchild that tries to leave the tree, wait for its record, exit.
#[allow(
    clippy::zombie_processes,
    reason = "the grandchild must outlive this fixture so the runner has to kill it"
)]
fn start_grandchild() -> i32 {
    let mut command = Command::new(env::current_exe().expect("fixture executable"));
    command
        .args(HARNESS_ARGS)
        .env(FIXTURE_ENV, "grandchild-sleep");
    command.spawn().expect("start grandchild");
    let record = PathBuf::from(env::var_os(RECORD_ENV).expect("record path"));
    let until = Instant::now() + Duration::from_secs(20);
    while !record.exists() && Instant::now() < until {
        thread::sleep(Duration::from_millis(10));
    }
    0
}

#[cfg(target_os = "linux")]
fn attempt_escape() -> String {
    match rustix::process::setsid() {
        Ok(_) => "allowed".to_owned(),
        Err(rustix::io::Errno::PERM) => "denied".to_owned(),
        Err(error) => format!("error-{error}"),
    }
}

#[cfg(windows)]
fn attempt_escape() -> String {
    use std::os::windows::process::CommandExt;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let attempt = Command::new(env::current_exe().expect("fixture executable"))
        .args(HARNESS_ARGS)
        .env(FIXTURE_ENV, "sleep")
        .creation_flags(CREATE_BREAKAWAY_FROM_JOB)
        .spawn();
    match attempt {
        Ok(mut escaped) => {
            let _unreportable = escaped.kill();
            "allowed".to_owned()
        }
        Err(_) => "denied".to_owned(),
    }
}

fn spec(mode: &str) -> ProcessSpec {
    let mut environment = vec![(OsString::from(FIXTURE_ENV), OsString::from(mode))];
    environment.extend(platform_environment());
    ProcessSpec {
        program: env::current_exe().expect("test executable"),
        arguments: HARNESS_ARGS.iter().map(OsString::from).collect(),
        working_directory: env::current_dir().expect("current directory"),
        environment,
    }
}

/// Windows programs commonly need `SystemRoot`; Linux needs nothing.
fn platform_environment() -> Vec<(OsString, OsString)> {
    #[cfg(windows)]
    if let Some(root) = env::var_os("SystemRoot") {
        return vec![(OsString::from("SystemRoot"), root)];
    }
    Vec::new()
}

fn with_record(mut spec: ProcessSpec, record: &Path) -> ProcessSpec {
    spec.environment
        .push((OsString::from(RECORD_ENV), record.as_os_str().to_owned()));
    spec
}

fn limits() -> ProcessLimits {
    ProcessLimits {
        deadline: Duration::from_secs(30),
        ..ProcessLimits::default()
    }
}

/// Limits that apply a disk bound to an otherwise roomy tree.
fn disk_limits(bytes: u64) -> ProcessLimits {
    ProcessLimits {
        deadline: Duration::from_secs(30),
        tree: Some(TreeLimits {
            max_memory_bytes: 1024 * 1024 * 1024,
            max_processes: 128,
            max_cpu_time: Duration::from_secs(60),
            max_disk_bytes: Some(bytes),
        }),
        ..ProcessLimits::default()
    }
}

/// Select the fixture mode that writes allocated bytes to `path`, then waits.
fn grow_spec(mut spec: ProcessSpec, path: &Path) -> ProcessSpec {
    spec.environment.retain(|(name, _)| name != FIXTURE_ENV);
    spec.environment
        .push((OsString::from(FIXTURE_ENV), OsString::from("grow-disk")));
    spec.environment
        .push((OsString::from(GROW_PATH_ENV), path.as_os_str().to_owned()));
    spec
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Marks a test process that already runs in its own delegated scope.
#[cfg(target_os = "linux")]
const DELEGATED_ENV: &str = "OMGCODE_DELEGATED_TEST";

/// Run `body` where tree limits can apply. On Linux, limits need a delegated
/// cgroup; unless this process already runs in one, the test re-runs itself
/// under `systemd-run --user --scope -p Delegate=yes`. It needs a user service
/// manager, and a missing manager fails the test rather than skipping it.
fn in_limit_scope(test: &str, body: impl FnOnce()) {
    #[cfg(target_os = "linux")]
    if env::var_os(DELEGATED_ENV).is_none() {
        let status = Command::new("systemd-run")
            .args(["--user", "--scope", "--quiet", "-p", "Delegate=yes", "--"])
            .arg(env::current_exe().expect("test executable"))
            .args(["--exact", test, "--nocapture", "--test-threads=1", "-q"])
            .env(DELEGATED_ENV, "1")
            .status()
            .expect("start systemd-run; tree-limit tests need a user service manager");
        assert!(status.success(), "{test} failed in its delegated scope");
        return;
    }
    let _ = test;
    body();
}

#[test]
fn exit_code_and_both_streams_are_captured() {
    let outcome = run(&spec("echo"), &limits(), &CancelSignal::new()).expect("run fixture");
    assert_eq!(outcome.termination, Termination::Exited { code: 7 });
    assert_eq!(outcome.confinement, AppliedConfinement::Unconfined);
    assert!(text(&outcome.stdout.bytes).contains("out-marker"));
    assert!(text(&outcome.stderr.bytes).contains("err-marker"));
    assert!(!outcome.stdout.truncated());
    assert!(!outcome.stderr.truncated());
}

#[test]
fn the_child_sees_exactly_the_supplied_environment() {
    let mut spec = spec("env");
    spec.environment
        .push((OsString::from("OMGCODE_TEST_VALUE"), OsString::from("1")));
    let expected: BTreeSet<String> = spec
        .environment
        .iter()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    let outcome = run(&spec, &limits(), &CancelSignal::new()).expect("run fixture");
    assert_eq!(outcome.termination, Termination::Exited { code: 0 });
    let seen: BTreeSet<String> = text(&outcome.stdout.bytes)
        .lines()
        .filter_map(|line| line.strip_prefix("ENV:").map(str::to_owned))
        .collect();
    assert_eq!(seen, expected);
}

#[test]
fn arguments_reach_the_child_unchanged() {
    let tricky = [
        "plain",
        "two words",
        "quote\"inside",
        "trailing\\",
        "back\\\\slash\\\"quote",
        "tab\there",
        "",
        "unicode-\u{e9}-\u{2713}",
    ];
    let mut spec = spec("args");
    spec.arguments.extend(tricky.iter().map(OsString::from));
    let outcome = run(&spec, &limits(), &CancelSignal::new()).expect("run fixture");
    assert_eq!(outcome.termination, Termination::Exited { code: 0 });
    let seen: Vec<String> = text(&outcome.stdout.bytes)
        .lines()
        .filter_map(|line| line.strip_prefix("ARG:").map(str::to_owned))
        .collect();
    let expected: Vec<String> = tricky
        .iter()
        .map(|argument| format!("{:?}", OsStr::new(argument)))
        .collect();
    assert_eq!(seen, expected);
}

#[test]
fn output_beyond_the_limit_is_drained_and_counted() {
    let limits = ProcessLimits {
        max_stdout_bytes: 4096,
        ..limits()
    };
    let outcome = run(&spec("flood"), &limits, &CancelSignal::new()).expect("run fixture");
    assert_eq!(outcome.termination, Termination::Exited { code: 0 });
    assert_eq!(outcome.stdout.bytes.len(), 4096);
    assert!(outcome.stdout.total_bytes >= u64::try_from(FLOOD_BYTES).expect("size"));
    assert!(outcome.stdout.truncated());
}

#[test]
fn the_deadline_terminates_the_tree() {
    let limits = ProcessLimits {
        deadline: Duration::from_millis(500),
        ..limits()
    };
    let outcome = run(&spec("sleep"), &limits, &CancelSignal::new()).expect("run fixture");
    assert_eq!(outcome.termination, Termination::DeadlineExceeded);
    assert!(outcome.elapsed < Duration::from_secs(20));
}

#[test]
fn cancellation_terminates_the_tree() {
    let cancel = CancelSignal::new();
    let trigger = cancel.clone();
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        trigger.cancel();
    });
    let outcome = run(&spec("sleep"), &limits(), &cancel).expect("run fixture");
    canceller.join().expect("canceller");
    assert_eq!(outcome.termination, Termination::Cancelled);
    assert!(outcome.elapsed < Duration::from_secs(20));
}

#[test]
fn descendants_cannot_escape_and_never_outlive_the_run() {
    let directory = tempfile::tempdir().expect("record directory");
    let record = directory.path().join("grandchild.txt");
    let spec = with_record(spec("grandchild"), &record);
    let outcome = run(&spec, &limits(), &CancelSignal::new()).expect("run fixture");
    let contents = fs::read_to_string(&record).expect("grandchild record");
    let pid = record_pid(&contents);
    let guard = KillOnDrop(pid);
    assert_eq!(outcome.termination, Termination::Exited { code: 0 });
    assert!(
        contents.contains("escape=denied"),
        "the grandchild escaped: {contents}"
    );
    assert!(
        exits_within(pid, Duration::from_secs(5)),
        "grandchild {pid} outlived the run"
    );
    drop(guard);
}

/// Whether `pid` is gone within `timeout`. Windows can report the job empty
/// before a killed member's process object is signaled, so an instant check
/// races the kernel's exit. A surviving grandchild sleeps for 120 seconds, so
/// this still fails when one escapes.
fn exits_within(pid: u32, timeout: Duration) -> bool {
    let until = Instant::now() + timeout;
    loop {
        if !is_alive(pid) {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_live_process_is_not_reported_gone() {
    assert!(!exits_within(std::process::id(), Duration::from_millis(50)));
}

#[test]
fn only_the_standard_streams_are_inherited() {
    let directory = tempfile::tempdir().expect("record directory");
    let record = directory.path().join("ready.txt");
    let (mut reader, writer) = std::io::pipe().expect("pipe");
    make_inheritable(&writer);
    let spec = with_record(spec("ready-sleep"), &record);
    let cancel = CancelSignal::new();
    let runner = {
        let cancel = cancel.clone();
        let limits = ProcessLimits {
            deadline: Duration::from_secs(10),
            ..limits()
        };
        thread::spawn(move || run(&spec, &limits, &cancel))
    };
    let until = Instant::now() + Duration::from_secs(20);
    while !record.exists() && Instant::now() < until {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(record.exists(), "the child did not start");
    drop(writer);
    let started = Instant::now();
    let mut contents = Vec::new();
    // End of file arrives at once unless the child also holds the writer.
    reader.read_to_end(&mut contents).expect("read pipe");
    let waited = started.elapsed();
    cancel.cancel();
    let outcome = runner.join().expect("runner").expect("run fixture");
    assert_eq!(outcome.termination, Termination::Cancelled);
    assert!(
        waited < Duration::from_secs(3),
        "the child inherited an unlisted handle ({waited:?})"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_signal_death_is_reported() {
    let outcome = run(&spec("signal"), &limits(), &CancelSignal::new()).expect("run fixture");
    assert_eq!(
        outcome.termination,
        Termination::Signalled {
            signal: libc::SIGTERM
        }
    );
}

#[test]
fn a_missing_program_fails_to_start() {
    let mut spec = spec("echo");
    let directory = spec.program.parent().expect("test directory").to_path_buf();
    spec.program = directory.join("omgcode-missing-program.exe");
    let error = run(&spec, &limits(), &CancelSignal::new()).expect_err("missing program");
    assert!(matches!(error, ProcessError::Spawn(_)), "{error}");
}

#[test]
fn unsafe_or_malformed_specifications_are_refused_before_start() {
    type Mutation = Box<dyn Fn(&mut ProcessSpec)>;
    let cases: Vec<(&str, Mutation)> = vec![
        (
            "relative program",
            Box::new(|spec| spec.program = PathBuf::from("tool.exe")),
        ),
        (
            "relative directory",
            Box::new(|spec| spec.working_directory = PathBuf::from("work")),
        ),
        (
            "NUL argument",
            Box::new(|spec| spec.arguments.push(OsString::from("a\0b"))),
        ),
        (
            "NUL value",
            Box::new(|spec| {
                spec.environment
                    .push((OsString::from("VALUE"), OsString::from("a\0b")));
            }),
        ),
        (
            "equals in name",
            Box::new(|spec| {
                spec.environment
                    .push((OsString::from("A=B"), OsString::from("1")));
            }),
        ),
        (
            "empty name",
            Box::new(|spec| {
                spec.environment
                    .push((OsString::new(), OsString::from("1")))
            }),
        ),
        (
            "duplicate name",
            Box::new(|spec| {
                spec.environment
                    .push((OsString::from(FIXTURE_ENV), OsString::from("again")));
            }),
        ),
        (
            "oversized arguments",
            Box::new(|spec| spec.arguments.push(OsString::from("x".repeat(200 * 1024)))),
        ),
    ];
    for (name, mutate) in cases {
        let mut spec = spec("echo");
        mutate(&mut spec);
        let error = run(&spec, &limits(), &CancelSignal::new()).expect_err(name);
        assert!(
            matches!(error, ProcessError::InvalidSpec(_)),
            "{name}: {error}"
        );
    }
}

#[test]
fn out_of_range_limits_are_refused_before_start() {
    let cases = [
        ProcessLimits {
            deadline: Duration::ZERO,
            ..limits()
        },
        ProcessLimits {
            deadline: MAX_DEADLINE + Duration::from_secs(1),
            ..limits()
        },
        ProcessLimits {
            cleanup_timeout: Duration::ZERO,
            ..limits()
        },
        ProcessLimits {
            cleanup_timeout: MAX_CLEANUP_TIMEOUT + Duration::from_secs(1),
            ..limits()
        },
        ProcessLimits {
            max_stderr_bytes: MAX_CAPTURE_BYTES + 1,
            ..limits()
        },
    ];
    for limits in cases {
        let error = run(&spec("echo"), &limits, &CancelSignal::new()).expect_err("limits");
        assert!(matches!(error, ProcessError::InvalidLimits(_)), "{error}");
    }
}

#[test]
fn malformed_confinement_is_refused_before_start() {
    let absolute = env::current_dir().expect("current directory");
    let cases = [
        (
            "relative read-only path",
            Confinement {
                read_only: vec![PathBuf::from("usr")],
                ..Confinement::default()
            },
        ),
        (
            "relative read-write path",
            Confinement {
                read_write: vec![PathBuf::from("workspace")],
                ..Confinement::default()
            },
        ),
        (
            "NUL in a path",
            Confinement {
                read_write: vec![absolute.join("a\0b")],
                ..Confinement::default()
            },
        ),
        (
            "too many paths",
            Confinement {
                read_only: vec![absolute.clone(); MAX_CONFINEMENT_PATHS],
                read_write: vec![absolute.clone()],
                ..Confinement::default()
            },
        ),
    ];
    for (name, confinement) in cases {
        let error = run_confined(&spec("echo"), &confinement, &limits(), &CancelSignal::new())
            .expect_err(name);
        assert!(
            matches!(error, ProcessError::InvalidSpec(_)),
            "{name}: {error}"
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_refuses_programs_that_are_not_plain_exe_files() {
    let directory = env::current_dir().expect("current directory");
    for name in [
        "tool.bat",
        "TOOL.CMD",
        "tool.exe.",
        "tool.exe ",
        "tool.exe:stream",
        "tool.com",
        "tool",
        "to\"ol.exe",
    ] {
        let mut spec = spec("echo");
        spec.program = directory.join(name);
        let error = run(&spec, &limits(), &CancelSignal::new()).expect_err(name);
        assert!(
            matches!(error, ProcessError::InvalidSpec(_)),
            "{name}: {error}"
        );
    }
    let mut spec = spec("echo");
    spec.program = PathBuf::from(r"\\.\pipe\tool.exe");
    let error = run(&spec, &limits(), &CancelSignal::new()).expect_err("device path");
    assert!(matches!(error, ProcessError::InvalidSpec(_)), "{error}");
}

#[cfg(windows)]
#[test]
fn windows_command_lines_follow_the_c_runtime_rules() {
    let spec = ProcessSpec {
        program: PathBuf::from(r"C:\Tools\tool.exe"),
        arguments: ["a b", "c\"d", "e\\", "", "f\\\"g", "h\\\\ i"]
            .iter()
            .map(OsString::from)
            .collect(),
        working_directory: PathBuf::from(r"C:\"),
        environment: Vec::new(),
    };
    let line = super::windows::command_line(&spec).expect("command line");
    let text = String::from_utf16(line.strip_suffix(&[0]).expect("NUL")).expect("UTF-16");
    assert_eq!(
        text,
        r#""C:\Tools\tool.exe" "a b" "c\"d" e\ "" "f\\\"g" "h\\ i""#
    );
}

fn record_pid(contents: &str) -> u32 {
    contents
        .lines()
        .find_map(|line| line.strip_prefix("pid="))
        .and_then(|pid| pid.parse().ok())
        .expect("recorded pid")
}

/// Kill a leaked fixture so a failing test does not leave it running.
struct KillOnDrop(u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if is_alive(self.0) {
            kill(self.0);
        }
    }
}

#[cfg(target_os = "linux")]
fn is_alive(pid: u32) -> bool {
    let pid = rustix::process::Pid::from_raw(i32::try_from(pid).expect("pid")).expect("pid");
    !matches!(
        rustix::process::test_kill_process(pid),
        Err(rustix::io::Errno::SRCH)
    )
}

#[cfg(target_os = "linux")]
fn kill(pid: u32) {
    let pid = rustix::process::Pid::from_raw(i32::try_from(pid).expect("pid")).expect("pid");
    let _unreportable = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
}

#[cfg(target_os = "linux")]
fn make_inheritable(writer: &std::io::PipeWriter) {
    rustix::io::fcntl_setfd(writer, rustix::io::FdFlags::empty()).expect("clear close-on-exec");
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn open_process(pid: u32) -> Option<std::os::windows::io::OwnedHandle> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };
    // SAFETY: plain value arguments; a null result means no such process.
    let handle = unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
            0,
            pid,
        )
    };
    if handle.is_null() {
        return None;
    }
    // SAFETY: the handle was just opened and is owned here.
    Some(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle) })
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn is_alive(pid: u32) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    let Some(process) = open_process(pid) else {
        return false;
    };
    // SAFETY: the process handle is live for the call.
    unsafe { WaitForSingleObject(process.as_raw_handle(), 0) == WAIT_TIMEOUT }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn kill(pid: u32) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Threading::TerminateProcess;
    if let Some(process) = open_process(pid) {
        // SAFETY: the process handle is live for the call.
        let _unreportable = unsafe { TerminateProcess(process.as_raw_handle(), 1) };
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn make_inheritable(writer: &std::io::PipeWriter) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    // SAFETY: the pipe handle is live and owned by `writer` for the call.
    let result = unsafe {
        SetHandleInformation(
            writer.as_raw_handle(),
            HANDLE_FLAG_INHERIT,
            HANDLE_FLAG_INHERIT,
        )
    };
    assert_ne!(result, 0, "make the pipe inheritable");
}
