//! Tree-limit tests. Each limit must end the whole run as `LimitExceeded`.
//!
//! On Linux, limits need a delegated cgroup. Unless this process already runs
//! in one prepared for these tests, each test re-runs itself under
//! `systemd-run --user --scope -p Delegate=yes`, so it needs a user service
//! manager. A missing manager fails the test rather than skipping it.

use std::env;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use super::{FIXTURE_ENV, HARNESS_ARGS, limits, spec};
use crate::process::{
    CancelSignal, ProcessError, ProcessLimits, ResourceLimit, Termination, TreeLimits,
    check_tree_limits, run,
};

/// Marks a test process that already runs in its own delegated scope.
#[cfg(target_os = "linux")]
const DELEGATED_ENV: &str = "OMGCODE_DELEGATED_TEST";

/// Generous limits that an ordinary fixture never reaches.
fn roomy() -> TreeLimits {
    TreeLimits {
        max_memory_bytes: 1024 * 1024 * 1024,
        max_processes: 256,
        max_cpu_time: Duration::from_secs(60),
    }
}

fn limited(tree: TreeLimits) -> ProcessLimits {
    ProcessLimits {
        tree: Some(tree),
        ..limits()
    }
}

/// Run `body` where tree limits can apply. See the module documentation.
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

/// Allocate and touch memory in 16 MiB steps, up to 4 GiB, then wait.
pub(super) fn hog_memory() -> i32 {
    const STEP: usize = 16 * 1024 * 1024;
    let mut held: Vec<Vec<u8>> = Vec::new();
    for _ in 0..256 {
        let mut block = Vec::new();
        if block.try_reserve_exact(STEP).is_err() {
            break;
        }
        block.resize(STEP, 1u8);
        held.push(block);
    }
    thread::sleep(Duration::from_secs(120));
    i32::try_from(held.len()).unwrap_or(i32::MAX)
}

/// Start up to 64 sleeping copies of the fixture, then wait. A refused start
/// is expected under a process limit.
#[allow(
    clippy::zombie_processes,
    reason = "the runner ends and reaps the whole tree, including these children"
)]
pub(super) fn spawn_many() -> i32 {
    for _ in 0..64 {
        let started = Command::new(env::current_exe().expect("fixture executable"))
            .args(HARNESS_ARGS)
            .env(FIXTURE_ENV, "sleep")
            .spawn();
        if started.is_err() {
            break;
        }
    }
    thread::sleep(Duration::from_secs(120));
    0
}

/// Keep one CPU busy for up to a minute.
pub(super) fn spin() -> i32 {
    let until = Instant::now() + Duration::from_secs(60);
    let mut counter = 0u64;
    while Instant::now() < until {
        counter = std::hint::black_box(counter.wrapping_add(1));
    }
    i32::from(counter == 0)
}

#[test]
fn a_limited_run_reports_its_usage() {
    in_limit_scope(
        "process::tests::tree_limits::a_limited_run_reports_its_usage",
        || {
            check_tree_limits().expect("tree limits are available");
            let outcome =
                run(&spec("echo"), &limited(roomy()), &CancelSignal::new()).expect("run fixture");
            assert_eq!(outcome.termination, Termination::Exited { code: 7 });
            let usage = outcome.usage.expect("a limited run reports usage");
            // Windows job accounting advances in clock ticks, so a short
            // program can report no CPU time there; cgroup usage is exact.
            #[cfg(target_os = "linux")]
            assert!(usage.cpu_time > Duration::ZERO);
            assert!(usage.cpu_time < roomy().max_cpu_time);
            let unlimited =
                run(&spec("echo"), &limits(), &CancelSignal::new()).expect("run fixture");
            assert_eq!(unlimited.usage, None);
        },
    );
}

#[test]
fn the_memory_limit_ends_the_run() {
    in_limit_scope(
        "process::tests::tree_limits::the_memory_limit_ends_the_run",
        || {
            let tree = TreeLimits {
                max_memory_bytes: 128 * 1024 * 1024,
                ..roomy()
            };
            let outcome =
                run(&spec("hog-memory"), &limited(tree), &CancelSignal::new()).expect("run");
            assert_eq!(
                outcome.termination,
                Termination::LimitExceeded {
                    limit: ResourceLimit::Memory
                }
            );
            assert!(outcome.elapsed < Duration::from_secs(20));
        },
    );
}

#[test]
fn the_process_limit_ends_the_run() {
    in_limit_scope(
        "process::tests::tree_limits::the_process_limit_ends_the_run",
        || {
            let tree = TreeLimits {
                max_processes: 8,
                ..roomy()
            };
            let outcome =
                run(&spec("spawn-many"), &limited(tree), &CancelSignal::new()).expect("run");
            assert_eq!(
                outcome.termination,
                Termination::LimitExceeded {
                    limit: ResourceLimit::Processes
                }
            );
            assert!(outcome.elapsed < Duration::from_secs(20));
        },
    );
}

#[test]
fn the_cpu_limit_ends_the_run() {
    in_limit_scope(
        "process::tests::tree_limits::the_cpu_limit_ends_the_run",
        || {
            let tree = TreeLimits {
                max_cpu_time: Duration::from_secs(1),
                ..roomy()
            };
            let outcome = run(&spec("spin"), &limited(tree), &CancelSignal::new()).expect("run");
            assert_eq!(
                outcome.termination,
                Termination::LimitExceeded {
                    limit: ResourceLimit::CpuTime
                }
            );
            let usage = outcome.usage.expect("usage");
            assert!(usage.cpu_time >= Duration::from_secs(1));
            assert!(outcome.elapsed < Duration::from_secs(20));
        },
    );
}

#[test]
fn out_of_range_tree_limits_start_nothing() {
    let cases = [
        TreeLimits {
            max_memory_bytes: 1024,
            ..roomy()
        },
        TreeLimits {
            max_memory_bytes: crate::process::MAX_TREE_MEMORY_BYTES + 1,
            ..roomy()
        },
        TreeLimits {
            max_processes: 0,
            ..roomy()
        },
        TreeLimits {
            max_processes: crate::process::MAX_TREE_PROCESSES + 1,
            ..roomy()
        },
        TreeLimits {
            max_cpu_time: Duration::from_millis(999),
            ..roomy()
        },
        TreeLimits {
            max_cpu_time: crate::process::MAX_TREE_CPU_TIME + Duration::from_secs(1),
            ..roomy()
        },
    ];
    for tree in cases {
        let result = run(&spec("echo"), &limited(tree), &CancelSignal::new());
        assert!(
            matches!(result, Err(ProcessError::InvalidLimits(_))),
            "{tree:?}"
        );
    }
}
