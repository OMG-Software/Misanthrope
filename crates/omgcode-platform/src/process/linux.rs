//! Linux process-tree supervision with a process group and seccomp.
//!
//! The program starts as the leader of a new process group. Before `exec`, the
//! child sets `PR_SET_PDEATHSIG`, marks every inherited descriptor above
//! standard error close-on-exec, sets `no_new_privs`, and installs a seccomp
//! filter that makes `setsid` and `setpgid` fail with `EPERM`. Descendants
//! inherit the filter, so none can leave the group, and `kill(-pgid)` reaches
//! the whole tree. No Landlock ruleset is applied yet; this is not filesystem
//! or network confinement.

use std::io;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use rustix::io::Errno;
use rustix::process::{Pid, Signal};

use super::{
    CancelSignal, Capture, ProcessError, ProcessLimits, ProcessOutcome, ProcessSpec, ProcessTree,
    Termination, stop_tree, supervise,
};

// Stable kernel ABI values for a classic BPF seccomp program, combined from
// the class, size, mode, and operation bits in `linux/bpf_common.h`.
/// `BPF_LD (0x00) | BPF_W (0x00) | BPF_ABS (0x20)`.
const BPF_LD_W_ABS: u16 = 0x20;
/// `BPF_JMP (0x05) | BPF_JEQ (0x10) | BPF_K (0x00)`.
const BPF_JMP_JEQ_K: u16 = 0x15;
/// `BPF_JMP (0x05) | BPF_JGE (0x30) | BPF_K (0x00)`.
#[cfg(target_arch = "x86_64")]
const BPF_JMP_JGE_K: u16 = 0x35;
/// `BPF_RET (0x06) | BPF_K (0x00)`.
const BPF_RET_K: u16 = 0x06;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_MODE_FILTER: libc::c_ulong = 2;
/// `offsetof(struct seccomp_data, nr)` and `offsetof(struct seccomp_data, arch)`.
const SECCOMP_DATA_NR: u32 = 0;
const SECCOMP_DATA_ARCH: u32 = 4;
const CLOSE_RANGE_CLOEXEC: libc::c_uint = 1 << 2;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;
/// System calls with this bit set use the x32 ABI, which the filter refuses.
#[cfg(target_arch = "x86_64")]
const X32_SYSCALL_BIT: u32 = 0x4000_0000;

pub(super) fn run(
    spec: &ProcessSpec,
    limits: ProcessLimits,
    cancel: &CancelSignal,
) -> Result<ProcessOutcome, ProcessError> {
    let filter = group_filter()?;
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
    // locks, and reads only the filter moved into the closure.
    #[allow(unsafe_code)]
    unsafe {
        command.pre_exec(move || prepare_child(parent, &filter));
    }
    let mut child = command.spawn().map_err(ProcessError::Spawn)?;
    let started = Instant::now();
    let group = Pid::from_child(&child);
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let mut tree = LinuxTree::new(child, group);
        stop_tree(&mut tree, limits.cleanup_timeout)?;
        tree.confirmed = true;
        return Err(ProcessError::Supervision(io::Error::other(
            "the child's output pipes were not created",
        )));
    };
    let mut tree = LinuxTree::new(child, group);
    let capture = match Capture::start(stdout, stderr, limits) {
        Ok(capture) => capture,
        Err(error) => {
            stop_tree(&mut tree, limits.cleanup_timeout)?;
            tree.confirmed = true;
            return Err(ProcessError::Supervision(error));
        }
    };
    let result = supervise(&mut tree, capture, started, limits, cancel);
    if !matches!(result, Err(ProcessError::CleanupUnconfirmed(_))) {
        tree.confirmed = true;
    }
    result
}

/// Steps run in the child between `fork` and `exec`.
fn prepare_child(parent: Pid, filter: &[libc::sock_filter]) -> io::Result<()> {
    rustix::process::set_parent_process_death_signal(Some(Signal::KILL))?;
    // The death signal is armed only for the current parent; if it already
    // exited, the child must not run unsupervised.
    if rustix::process::getppid() != Some(parent) {
        return Err(io::Error::from_raw_os_error(libc::ESRCH));
    }
    mark_descriptors_close_on_exec()?;
    set_no_new_privs()?;
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

const fn statement(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

const fn jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

fn syscall_number(number: libc::c_long) -> Result<u32, ProcessError> {
    u32::try_from(number)
        .map_err(|_| ProcessError::Unsupported("system call number is out of range"))
}

/// A filter that denies leaving the process group and kills any other ABI.
#[cfg(target_arch = "x86_64")]
fn group_filter() -> Result<Vec<libc::sock_filter>, ProcessError> {
    let setsid = syscall_number(libc::SYS_setsid)?;
    let setpgid = syscall_number(libc::SYS_setpgid)?;
    Ok(vec![
        statement(BPF_LD_W_ABS, SECCOMP_DATA_ARCH),
        jump(BPF_JMP_JEQ_K, AUDIT_ARCH, 1, 0),
        statement(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
        statement(BPF_LD_W_ABS, SECCOMP_DATA_NR),
        jump(BPF_JMP_JGE_K, X32_SYSCALL_BIT, 0, 1),
        statement(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
        jump(BPF_JMP_JEQ_K, setsid, 2, 0),
        jump(BPF_JMP_JEQ_K, setpgid, 1, 0),
        statement(BPF_RET_K, SECCOMP_RET_ALLOW),
        statement(BPF_RET_K, SECCOMP_RET_ERRNO | errno_data(libc::EPERM)),
    ])
}

/// A filter that denies leaving the process group and kills any other ABI.
#[cfg(target_arch = "aarch64")]
fn group_filter() -> Result<Vec<libc::sock_filter>, ProcessError> {
    let setsid = syscall_number(libc::SYS_setsid)?;
    let setpgid = syscall_number(libc::SYS_setpgid)?;
    Ok(vec![
        statement(BPF_LD_W_ABS, SECCOMP_DATA_ARCH),
        jump(BPF_JMP_JEQ_K, AUDIT_ARCH, 1, 0),
        statement(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
        statement(BPF_LD_W_ABS, SECCOMP_DATA_NR),
        jump(BPF_JMP_JEQ_K, setsid, 2, 0),
        jump(BPF_JMP_JEQ_K, setpgid, 1, 0),
        statement(BPF_RET_K, SECCOMP_RET_ALLOW),
        statement(BPF_RET_K, SECCOMP_RET_ERRNO | errno_data(libc::EPERM)),
    ])
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn group_filter() -> Result<Vec<libc::sock_filter>, ProcessError> {
    Err(ProcessError::Unsupported(
        "the process-group filter supports only x86_64 and aarch64",
    ))
}

const fn errno_data(errno: libc::c_int) -> u32 {
    // SECCOMP_RET_DATA is the low 16 bits; EPERM fits.
    (errno as u32) & 0xffff
}

struct LinuxTree {
    child: Child,
    group: Pid,
    reaped: bool,
    /// Set once the tree was observed empty; `Drop` then has nothing to do.
    confirmed: bool,
}

impl LinuxTree {
    fn new(child: Child, group: Pid) -> Self {
        Self {
            child,
            group,
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
        match rustix::process::kill_process_group(self.group, Signal::KILL) {
            Ok(()) | Err(Errno::SRCH) => Ok(()),
            Err(error) => Err(error.into()),
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
