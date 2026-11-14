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

use super::{FIXTURE_ENV, HARNESS_ARGS, in_limit_scope, limits, spec};
use crate::process::{
    CancelSignal, ProcessError, ProcessLimits, ResourceLimit, Termination, TreeLimits,
    check_tree_limits, run,
};

/// Generous limits that an ordinary fixture never reaches.
fn roomy() -> TreeLimits {
    TreeLimits {
        max_memory_bytes: 1024 * 1024 * 1024,
        max_processes: 256,
        max_cpu_time: Duration::from_secs(60),
        max_disk_bytes: None,
    }
}

fn limited(tree: TreeLimits) -> ProcessLimits {
    ProcessLimits {
        tree: Some(tree),
        ..limits()
    }
}

/// Allocate and touch memory in 1 MiB steps, up to `limit` MiB or the first
/// refused allocation, and return what was allocated. Small steps bring a
/// Windows job's peak commit close to its limit, as most real breaches do.
fn fill_memory(limit: usize) -> Vec<Vec<u8>> {
    const STEP: usize = 1024 * 1024;
    let mut held: Vec<Vec<u8>> = Vec::new();
    for _ in 0..limit {
        let mut block = Vec::new();
        if block.try_reserve_exact(STEP).is_err() {
            break;
        }
        block.resize(STEP, 1u8);
        held.push(block);
    }
    held
}

/// Allocate memory up to 4 GiB, then wait.
pub(super) fn hog_memory() -> i32 {
    let held = fill_memory(4096);
    thread::sleep(Duration::from_secs(120));
    i32::try_from(held.len()).unwrap_or(i32::MAX)
}

/// Join a nested job with stricter limits than the run's, reach both of
/// them, then exit with 7. Each breach is reported to the run's port too.
/// It exits itself, so it never returns.
#[cfg(windows)]
#[allow(
    clippy::zombie_processes,
    reason = "the runner ends and reaps the whole tree, including these children"
)]
pub(super) fn nested_job() -> i32 {
    let job = crate::process::windows::join_nested_job(TreeLimits {
        max_memory_bytes: 32 * 1024 * 1024,
        max_processes: 2,
        max_cpu_time: Duration::from_secs(60),
        max_disk_bytes: None,
    })
    .expect("join a nested job");
    let held = fill_memory(4096);
    assert!(held.len() < 32, "the nested memory limit did not apply");
    drop(held);
    let mut refused = false;
    for _ in 0..4 {
        let started = Command::new(env::current_exe().expect("fixture executable"))
            .args(HARNESS_ARGS)
            .env(FIXTURE_ENV, "sleep")
            .spawn();
        if started.is_err() {
            refused = true;
            break;
        }
    }
    assert!(refused, "the nested process limit did not apply");
    // Exit while the nested job is still open: closing its handle would kill
    // this process, because the job kills its members on close.
    std::io::Write::flush(&mut std::io::stderr()).expect("flush stderr");
    let _keep = job;
    std::process::exit(7)
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

/// A tool's own nested job with stricter limits reports its breaches to
/// the run's port as well. They must not end the run as the run's limit.
#[cfg(windows)]
#[test]
fn a_nested_job_limit_is_not_the_runs_limit() {
    let outcome =
        run(&spec("nested-job"), &limited(roomy()), &CancelSignal::new()).expect("run fixture");
    assert_eq!(
        outcome.termination,
        Termination::Exited { code: 7 },
        "stdout={} stderr={}",
        String::from_utf8_lossy(&outcome.stdout.bytes),
        String::from_utf8_lossy(&outcome.stderr.bytes)
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
