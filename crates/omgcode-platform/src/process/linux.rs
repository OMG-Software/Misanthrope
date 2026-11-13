//! Linux process-tree supervision with a process group, seccomp, and Landlock.
//!
//! The program starts as the leader of a new process group. Before `exec`, the
//! child sets `PR_SET_PDEATHSIG`, marks every inherited descriptor above
//! standard error close-on-exec, sets `no_new_privs`, applies the Landlock
//! ruleset for a confined run, and installs a seccomp filter. The filter makes
//! `setsid` and `setpgid` fail with `EPERM`, so `kill(-pgid)` reaches the whole
//! tree; for a confined run it also refuses sockets and `io_uring`. Descendants
//! inherit both the ruleset and the filter. A run with tree limits first joins
//! its own cgroup, which also kills and observes the tree.

pub(super) mod cgroup;
mod landlock;
mod seccomp;

use std::io;
use std::os::fd::RawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use rustix::io::Errno;
use rustix::process::{Pid, Signal};

use self::cgroup::RunCgroup;
use self::landlock::Ruleset;
use self::seccomp::FilterScope;
use super::{
    AppliedConfinement, CancelSignal, Capture, Confinement, DiskMonitor, ProcessError,
    ProcessLimits, ProcessOutcome, ProcessSpec, ProcessTree, ResourceLimit, Termination, TreeUsage,
    stop_tree, supervise,
};

const SECCOMP_MODE_FILTER: libc::c_ulong = 2;
const CLOSE_RANGE_CLOEXEC: libc::c_uint = 1 << 2;

pub(super) fn run(
    spec: &ProcessSpec,
    confinement: Option<&Confinement>,
    limits: ProcessLimits,
    cancel: &CancelSignal,
    mut disk: Option<DiskMonitor>,
) -> Result<ProcessOutcome, ProcessError> {
    let (filter, ruleset, applied) = match confinement {
        None => (
            seccomp::filter(FilterScope::ProcessGroup)?,
            None,
            AppliedConfinement::Unconfined,
        ),
        Some(confinement) => {
            let filter = seccomp::filter(FilterScope::Confined {
                unix_sockets: confinement.unix_sockets,
                network: confinement.network,
            })?;
            let ruleset = Ruleset::prepare(confinement)?;
            let applied = AppliedConfinement::Landlock {
                abi: ruleset.abi(),
                unix_sockets: confinement.unix_sockets,
                network: confinement.network,
            };
            (filter, Some(ruleset), applied)
        }
    };
    let cgroup = limits.tree.map(RunCgroup::create).transpose()?;
    // The parent keeps `ruleset` and the cgroup open until `spawn` returns;
    // the child uses its inherited copies, which close at `exec`.
    let ruleset_fd = ruleset.as_ref().map(Ruleset::raw_fd);
    let cgroup_fd = cgroup.as_ref().map(RunCgroup::procs_fd);
    let parent = rustix::process::getpid();
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.arguments)
        .current_dir(&spec.working_directory)
        .env_clear()
        .envs(spec.environment.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // `std` calls setpgid before the hook below installs the filter.
        .process_group(0);
    // SAFETY: the hook runs in the forked child before `exec`. It only makes
    // raw system calls through rustix and libc, allocates nothing, takes no
    // locks, and reads only the filter and descriptor number moved into the
    // closure.
    #[allow(unsafe_code)]
    unsafe {
        command.pre_exec(move || prepare_child(parent, &filter, ruleset_fd, cgroup_fd));
    }
    // Start sampling before the program exists, so a sampler that cannot start
    // starts nothing.
    if let Some(disk) = &mut disk {
        disk.start()?;
    }
    let spawned = command.spawn();
    drop(ruleset);
    let mut child = spawned.map_err(ProcessError::Spawn)?;
    let started = Instant::now();
    let group = Pid::from_child(&child);
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let mut tree = LinuxTree::new(child, group, cgroup);
        stop_tree(&mut tree, limits.cleanup_timeout)?;
        tree.confirmed = true;
        return Err(ProcessError::Supervision(io::Error::other(
            "the child's output pipes were not created",
        )));
    };
    let mut tree = LinuxTree::new(child, group, cgroup);
    let capture = match Capture::start(stdout, stderr, limits) {
        Ok(capture) => capture,
        Err(error) => {
            stop_tree(&mut tree, limits.cleanup_timeout)?;
            tree.confirmed = true;
            return Err(ProcessError::Supervision(error));
        }
    };
    let result = supervise(&mut tree, capture, started, limits, cancel, applied, disk);
    if !matches!(result, Err(ProcessError::CleanupUnconfirmed(_))) {
        tree.confirmed = true;
    }
    result
}

/// Steps run in the child between `fork` and `exec`. The cgroup comes first,
/// so every later step and every descendant is counted. Landlock follows
/// `no_new_privs`, which it requires, and precedes the seccomp filter.
fn prepare_child(
    parent: Pid,
    filter: &[libc::sock_filter],
    ruleset: Option<RawFd>,
    cgroup: Option<RawFd>,
) -> io::Result<()> {
    if let Some(procs) = cgroup {
        cgroup::join(procs)?;
    }
    rustix::process::set_parent_process_death_signal(Some(Signal::KILL))?;
    // The death signal is armed only for the current parent; if it already
    // exited, the child must not run unsupervised.
    if rustix::process::getppid() != Some(parent) {
        return Err(io::Error::from_raw_os_error(libc::ESRCH));
    }
    mark_descriptors_close_on_exec()?;
    set_no_new_privs()?;
    if let Some(ruleset) = ruleset {
        landlock::restrict_self(ruleset)?;
    }
    install_filter(filter)
}

#[allow(unsafe_code)]
fn set_no_new_privs() -> io::Result<()> {
    // SAFETY: PR_SET_NO_NEW_PRIVS takes only integer arguments and changes
    // only this process's privilege flag; it touches no memory.
    let result = unsafe {
        libc::prctl(
            libc::PR_SET_NO_NEW_PRIVS,
            1 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[allow(unsafe_code)]
fn mark_descriptors_close_on_exec() -> io::Result<()> {
    // SAFETY: `close_range` with CLOSE_RANGE_CLOEXEC only sets a flag on open
    // descriptors from 3 upward; it closes nothing and touches no memory.
    let result = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3 as libc::c_uint,
            libc::c_uint::MAX,
            CLOSE_RANGE_CLOEXEC,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[allow(unsafe_code)]
fn install_filter(filter: &[libc::sock_filter]) -> io::Result<()> {
    let length =
        u16::try_from(filter.len()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let program = libc::sock_fprog {
        len: length,
        filter: filter.as_ptr().cast_mut(),
    };
    // SAFETY: `program` points to `length` valid instructions that outlive the
    // call; the kernel copies the program and does not write through it.
    let result = unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            SECCOMP_MODE_FILTER,
            &raw const program,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

struct LinuxTree {
    child: Child,
    group: Pid,
    /// The run's cgroup when tree limits apply.
    cgroup: Option<RunCgroup>,
    reaped: bool,
    /// Set once the tree was observed empty; `Drop` then has nothing to do.
    confirmed: bool,
}

impl LinuxTree {
    fn new(child: Child, group: Pid, cgroup: Option<RunCgroup>) -> Self {
        Self {
            child,
            group,
            cgroup,
            reaped: false,
            confirmed: false,
        }
    }
}

impl ProcessTree for LinuxTree {
    fn try_exit(&mut self) -> io::Result<Option<Termination>> {
        let Some(status) = self.child.try_wait()? else {
            return Ok(None);
        };
        self.reaped = true;
        if let Some(code) = status.code() {
            return Ok(Some(Termination::Exited {
                code: i64::from(code),
            }));
        }
        if let Some(signal) = status.signal() {
            return Ok(Some(Termination::Signalled { signal }));
        }
        Err(io::Error::other(
            "the program ended without a code or signal",
        ))
    }

    fn terminate(&mut self) -> io::Result<()> {
        // Try both, so a failed cgroup kill still kills the process group;
        // then report the first failure.
        let cgroup = self.cgroup.as_ref().map_or(Ok(()), RunCgroup::kill);
        let group = match rustix::process::kill_process_group(self.group, Signal::KILL) {
            Ok(()) | Err(Errno::SRCH) => Ok(()),
            Err(error) => Err(error.into()),
        };
        cgroup.and(group)
    }

    fn limit_reached(&mut self) -> io::Result<Option<ResourceLimit>> {
        match &mut self.cgroup {
            Some(cgroup) => cgroup.limit_reached(),
            None => Ok(None),
        }
    }

    fn usage(&mut self) -> io::Result<Option<TreeUsage>> {
        self.cgroup.as_ref().map(RunCgroup::usage).transpose()
    }

    fn release(&mut self) -> Result<(), ProcessError> {
        match &mut self.cgroup {
            Some(cgroup) => cgroup.remove().map_err(|_| {
                ProcessError::CleanupUnconfirmed("the run's cgroup could not be removed")
            }),
            None => Ok(()),
        }
    }

    fn is_empty(&mut self) -> io::Result<bool> {
        // The unreaped leader keeps the group, and its number, alive.
        if !self.reaped {
            if self.child.try_wait()?.is_none() {
                return Ok(false);
            }
            self.reaped = true;
        }
        if let Some(cgroup) = &self.cgroup
            && !cgroup.is_empty()?
        {
            return Ok(false);
        }
        match rustix::process::test_kill_process_group(self.group) {
            Err(Errno::SRCH) => Ok(true),
            // EPERM still means a member exists.
            Ok(()) | Err(Errno::PERM) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}

impl Drop for LinuxTree {
    fn drop(&mut self) {
        if self.confirmed {
            return;
        }
        // Best effort on an unconfirmed or unwinding path. Errors are not
        // reportable from `Drop`; the caller already received an error.
        let _unreportable = self.terminate();
        if !self.reaped {
            let _unreportable = self.child.try_wait();
        }
    }
}
