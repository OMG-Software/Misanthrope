//! Process-tree runner core.
//!
//! This module starts one program with an explicit environment and supervises
//! its whole descendant tree: bounded output, a deadline, cooperative
//! cancellation, observed tree cleanup, and tree-wide memory, process, CPU-time,
//! and disk limits. [`run`] applies no filesystem or network confinement.
//! [`run_confined`] adds a Landlock filesystem allowlist and a seccomp socket
//! filter on Linux, and an AppContainer with recorded directory grants on
//! Windows. The disk limit samples the bytes allocated under the writable
//! roots on a separate thread; neither platform offers a per-run disk quota.
//! See `docs/execution.md`, ADR-0027, ADR-0028, ADR-0029, ADR-0031, and
//! ADR-0033.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

mod confinement;
mod disk;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

use disk::DiskMonitor;

#[cfg(test)]
mod tests;

#[cfg(target_os = "linux")]
pub use confinement::network_support_paths;
pub use confinement::{
    AppliedConfinement, Confinement, MAX_CONFINEMENT_PATHS, NetworkAccess, UnixSocketAccess,
};

/// Largest accepted run deadline.
pub const MAX_DEADLINE: Duration = Duration::from_secs(60 * 60);
/// Largest accepted wait for observed tree cleanup.
pub const MAX_CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest number of output bytes retained from one stream.
pub const MAX_CAPTURE_BYTES: usize = 16 * 1024 * 1024;
/// Largest number of program arguments.
pub const MAX_ARGUMENTS: usize = 4096;
/// Largest number of environment entries.
pub const MAX_ENVIRONMENT_ENTRIES: usize = 512;
/// Largest total size of arguments plus environment, in bytes of OS text.
pub const MAX_COMMAND_BYTES: usize = 128 * 1024;
/// Smallest accepted tree memory limit.
pub const MIN_TREE_MEMORY_BYTES: u64 = 16 * 1024 * 1024;
/// Largest accepted tree memory limit.
pub const MAX_TREE_MEMORY_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
/// Largest accepted tree process limit.
pub const MAX_TREE_PROCESSES: u32 = 65_536;
/// Largest accepted tree CPU-time limit.
pub const MAX_TREE_CPU_TIME: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Smallest accepted tree disk limit.
pub const MIN_TREE_DISK_BYTES: u64 = 16 * 1024 * 1024;
/// Largest accepted tree disk limit.
pub const MAX_TREE_DISK_BYTES: u64 = 1024 * 1024 * 1024 * 1024;

/// Poll interval for exit, deadline, and cancellation checks.
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Read buffer for one captured stream.
const READ_CHUNK: usize = 8 * 1024;

/// One program to start. Nothing is inherited from the calling process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessSpec {
    /// Absolute path of the program. The runner never searches `PATH`.
    pub program: PathBuf,
    /// Arguments after the program name.
    pub arguments: Vec<OsString>,
    /// Absolute working directory.
    pub working_directory: PathBuf,
    /// The complete environment. The runner adds nothing.
    pub environment: Vec<(OsString, OsString)>,
}

/// Bounds for one run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessLimits {
    /// Time from start until the tree is terminated.
    pub deadline: Duration,
    /// Bytes retained from standard output; the rest is drained and counted.
    pub max_stdout_bytes: usize,
    /// Bytes retained from standard error; the rest is drained and counted.
    pub max_stderr_bytes: usize,
    /// Time allowed to observe an empty tree and closed output after termination.
    pub cleanup_timeout: Duration,
    /// Limits for the whole tree. `None` applies none. When set, a limit that
    /// cannot be applied starts nothing.
    pub tree: Option<TreeLimits>,
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self {
            deadline: Duration::from_secs(60),
            max_stdout_bytes: 1024 * 1024,
            max_stderr_bytes: 1024 * 1024,
            cleanup_timeout: Duration::from_secs(5),
            tree: None,
        }
    }
}

/// Resource limits for a whole process tree. See `docs/execution.md`.
///
/// The kernel enforces memory and process counts: on Windows through Job
/// Object limits, on Linux through a child of the runner's delegated cgroup
/// v2. The supervisor enforces CPU time by polling the tree's accounting and
/// disk growth by sampling the writable roots. When any limit is reached, the
/// runner terminates the whole tree and reports
/// [`Termination::LimitExceeded`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeLimits {
    /// Memory for the whole tree. Windows counts committed private memory;
    /// Linux counts the cgroup's memory, including page cache, with no swap.
    pub max_memory_bytes: u64,
    /// Processes alive at once. Linux counts threads as well, because the
    /// cgroup `pids` controller counts tasks.
    pub max_processes: u32,
    /// User and kernel CPU time summed over every process the tree started.
    pub max_cpu_time: Duration,
    /// Greatest net growth the tree may add to its writable roots, measured
    /// as allocated bytes and as file lengths; passing either budget ends the
    /// run. `None` applies no disk limit; a confined run with a limit must
    /// name at least one writable path. This is sampled, not a filesystem
    /// quota; see [`run_confined`] and `docs/execution.md`.
    pub max_disk_bytes: Option<u64>,
}

impl TreeLimits {
    fn validate(self) -> Result<Self, ProcessError> {
        if !(MIN_TREE_MEMORY_BYTES..=MAX_TREE_MEMORY_BYTES).contains(&self.max_memory_bytes) {
            return Err(ProcessError::InvalidLimits(
                "tree memory limit is out of range",
            ));
        }
        if self.max_processes == 0 || self.max_processes > MAX_TREE_PROCESSES {
            return Err(ProcessError::InvalidLimits(
                "tree process limit is out of range",
            ));
        }
        if self.max_cpu_time < Duration::from_secs(1) || self.max_cpu_time > MAX_TREE_CPU_TIME {
            return Err(ProcessError::InvalidLimits(
                "tree CPU-time limit is out of range",
            ));
        }
        if let Some(disk) = self.max_disk_bytes
            && !(MIN_TREE_DISK_BYTES..=MAX_TREE_DISK_BYTES).contains(&disk)
        {
            return Err(ProcessError::InvalidLimits(
                "tree disk limit is out of range",
            ));
        }
        Ok(self)
    }
}

/// The tree limit that ended a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceLimit {
    /// The tree reached its memory limit.
    Memory,
    /// The tree tried to exceed its process limit.
    Processes,
    /// The tree used its CPU time.
    CpuTime,
    /// The tree's writable roots grew past the disk limit.
    Disk,
}

/// Resources a limited tree used, read after it was observed empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeUsage {
    /// User and kernel CPU time summed over the tree.
    pub cpu_time: Duration,
    /// Peak memory, when the platform reports it: the Windows job's peak
    /// commit, or Linux `memory.peak` (Linux 5.19 or later).
    pub peak_memory_bytes: Option<u64>,
    /// Highest net growth of the writable roots above the pre-run baseline,
    /// when a disk limit applied. It is the larger of allocated-byte growth
    /// and file-length growth.
    pub peak_disk_bytes: Option<u64>,
}

impl ProcessLimits {
    /// Check every bound against the runner's ranges, including the tree
    /// limits when present. [`run`] and [`run_confined`] check this as well;
    /// callers that record a run first check it before recording.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::InvalidLimits`] for a value out of range.
    pub fn validate(self) -> Result<Self, ProcessError> {
        if self.deadline.is_zero() || self.deadline > MAX_DEADLINE {
            return Err(ProcessError::InvalidLimits("deadline is out of range"));
        }
        if self.cleanup_timeout.is_zero() || self.cleanup_timeout > MAX_CLEANUP_TIMEOUT {
            return Err(ProcessError::InvalidLimits(
                "cleanup timeout is out of range",
            ));
        }
        if self.max_stdout_bytes > MAX_CAPTURE_BYTES || self.max_stderr_bytes > MAX_CAPTURE_BYTES {
            return Err(ProcessError::InvalidLimits(
                "output capture limit is too large",
            ));
        }
        if let Some(tree) = self.tree {
            tree.validate()?;
        }
        Ok(self)
    }
}

/// A cancellation request shared with another thread.
#[derive(Clone, Debug, Default)]
pub struct CancelSignal(Arc<AtomicBool>);

impl CancelSignal {
    /// Create a signal that has not been raised.
    pub fn new() -> Self {
        Self::default()
    }

    /// Request termination of the running tree at the next poll.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// How the run ended. Every variant means the whole tree was observed empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Termination {
    /// The program exited with this code. Windows codes are `u32` values.
    Exited { code: i64 },
    /// The program was ended by this Unix signal before supervision stopped it.
    Signalled { signal: i32 },
    /// The deadline elapsed and the runner terminated the tree.
    DeadlineExceeded,
    /// Cancellation was requested and the runner terminated the tree.
    Cancelled,
    /// The tree reached a [`TreeLimits`] limit and the runner terminated it.
    /// This takes precedence over the program's own exit.
    LimitExceeded { limit: ResourceLimit },
}

/// Bytes captured from one output stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapturedOutput {
    /// Retained bytes, at most the stream's limit.
    pub bytes: Vec<u8>,
    /// Every byte read from the stream, including discarded bytes.
    pub total_bytes: u64,
}

impl CapturedOutput {
    /// Whether some output was discarded after the retention limit.
    pub fn truncated(&self) -> bool {
        u64::try_from(self.bytes.len()).map_or(true, |kept| kept < self.total_bytes)
    }
}

/// The result of one supervised run.
#[derive(Clone, Debug, PartialEq, Eq)]
#[must_use]
pub struct ProcessOutcome {
    pub termination: Termination,
    /// The confinement that applied to every process in the tree.
    pub confinement: AppliedConfinement,
    pub stdout: CapturedOutput,
    pub stderr: CapturedOutput,
    pub elapsed: Duration,
    /// What a limited tree used; `None` when no tree limits applied.
    pub usage: Option<TreeUsage>,
}

/// A controlled runner failure. No variant reports a successful run.
#[derive(Debug)]
pub enum ProcessError {
    /// The specification is unsafe or malformed; nothing was started.
    InvalidSpec(&'static str),
    /// The limits are out of range; nothing was started.
    InvalidLimits(&'static str),
    /// A required native primitive is unavailable; nothing was started.
    Unsupported(&'static str),
    /// A confinement rule could not be prepared, for example because a path is
    /// missing, is a symbolic link, or has an unsupported type. Nothing was
    /// started.
    Confinement(io::Error),
    /// The program could not be started. No process from this run remains.
    Spawn(io::Error),
    /// Supervision failed after start. The runner terminated the tree and
    /// observed it empty before returning this error.
    Supervision(io::Error),
    /// The runner could not observe the tree empty, or its output closed,
    /// within the cleanup timeout. Processes may remain.
    CleanupUnconfirmed(&'static str),
    /// A disk limit could not be prepared or its sampler could not start.
    /// Nothing was started.
    DiskMeasurement(io::Error),
}

impl fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSpec(reason) => {
                write!(formatter, "invalid process specification: {reason}")
            }
            Self::InvalidLimits(reason) => write!(formatter, "invalid process limits: {reason}"),
            Self::Unsupported(reason) => write!(formatter, "process runner unavailable: {reason}"),
            Self::Confinement(error) => {
                write!(
                    formatter,
                    "a confinement rule could not be prepared: {error}"
                )
            }
            Self::Spawn(error) => write!(formatter, "the program could not be started: {error}"),
            Self::Supervision(error) => write!(formatter, "process supervision failed: {error}"),
            Self::CleanupUnconfirmed(reason) => {
                write!(
                    formatter,
                    "process tree cleanup was not confirmed: {reason}"
                )
            }
            Self::DiskMeasurement(error) => {
                write!(formatter, "the disk limit could not be applied: {error}")
            }
        }
    }
}

impl std::error::Error for ProcessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error)
            | Self::Supervision(error)
            | Self::Confinement(error)
            | Self::DiskMeasurement(error) => Some(error),
            _ => None,
        }
    }
}

/// Start one program and supervise its whole tree until it is observed empty.
///
/// The tree is terminated when the program exits, when the deadline elapses, or
/// when `cancel` is raised; descendants never outlive a run that returns `Ok`.
/// This applies no filesystem or network confinement.
pub fn run(
    spec: &ProcessSpec,
    limits: &ProcessLimits,
    cancel: &CancelSignal,
) -> Result<ProcessOutcome, ProcessError> {
    let limits = limits.validate()?;
    validate_spec(spec)?;
    if limits
        .tree
        .is_some_and(|tree| tree.max_disk_bytes.is_some())
    {
        return Err(ProcessError::InvalidLimits(
            "a disk limit needs a confined run",
        ));
    }
    run_native(spec, None, limits, cancel, None)
}

/// Start one program under filesystem and network confinement and supervise
/// its whole tree until it is observed empty.
///
/// The program and every descendant can reach only the paths in
/// `confinement`, cannot create IP sockets, and cannot use `io_uring`. By
/// default they can create only a Unix `socketpair` of stream or
/// sequenced-packet type; [`UnixSocketAccess::HostServices`] also allows
/// connecting to host Unix-domain sockets. Setup fails closed: if a rule or primitive cannot be applied,
/// nothing starts and an error is returned. On Linux this needs Landlock ABI 3
/// or later. On Windows the tree runs in the AppContainer named by
/// [`Confinement::container`] with no capabilities, so it has no network
/// access; each listed path, and the program, must already be granted to it.
/// Windows refuses [`UnixSocketAccess::HostServices`] with
/// [`ProcessError::Unsupported`].
pub fn run_confined(
    spec: &ProcessSpec,
    confinement: &Confinement,
    limits: &ProcessLimits,
    cancel: &CancelSignal,
) -> Result<ProcessOutcome, ProcessError> {
    let limits = limits.validate()?;
    validate_spec(spec)?;
    confinement.validate()?;
    let disk = DiskMonitor::prepare(
        Some(confinement),
        limits.tree.and_then(|tree| tree.max_disk_bytes),
    )?;
    run_native(spec, Some(confinement), limits, cancel, disk)
}

/// Check that [`TreeLimits`] can be applied here, so a caller can refuse a
/// run before recording it.
///
/// On Linux this needs a delegated cgroup v2 and, on first use, moves the
/// calling process into its `omgcode-supervisor` leaf and enables the
/// `memory` and `pids` controllers for the delegated cgroup's children. On
/// Windows, Job Object limits are always available.
pub fn check_tree_limits() -> Result<(), ProcessError> {
    #[cfg(target_os = "linux")]
    {
        linux::cgroup::ensure_available()
    }
    #[cfg(windows)]
    {
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        Err(ProcessError::Unsupported(
            "tree limits are implemented only on Linux and Windows",
        ))
    }
}

#[cfg(target_os = "linux")]
fn run_native(
    spec: &ProcessSpec,
    confinement: Option<&Confinement>,
    limits: ProcessLimits,
    cancel: &CancelSignal,
    disk: Option<DiskMonitor>,
) -> Result<ProcessOutcome, ProcessError> {
    linux::run(spec, confinement, limits, cancel, disk)
}

#[cfg(windows)]
fn run_native(
    spec: &ProcessSpec,
    confinement: Option<&Confinement>,
    limits: ProcessLimits,
    cancel: &CancelSignal,
    disk: Option<DiskMonitor>,
) -> Result<ProcessOutcome, ProcessError> {
    windows::run(spec, confinement, limits, cancel, disk)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn run_native(
    _spec: &ProcessSpec,
    _confinement: Option<&Confinement>,
    _limits: ProcessLimits,
    _cancel: &CancelSignal,
    _disk: Option<DiskMonitor>,
) -> Result<ProcessOutcome, ProcessError> {
    Err(ProcessError::Unsupported(
        "process supervision is implemented only on Linux and Windows",
    ))
}

fn validate_spec(spec: &ProcessSpec) -> Result<(), ProcessError> {
    if !spec.program.is_absolute() {
        return Err(ProcessError::InvalidSpec("program path must be absolute"));
    }
    if !spec.working_directory.is_absolute() {
        return Err(ProcessError::InvalidSpec(
            "working directory must be absolute",
        ));
    }
    if contains_nul(spec.program.as_os_str()) || contains_nul(spec.working_directory.as_os_str()) {
        return Err(ProcessError::InvalidSpec("paths must not contain NUL"));
    }
    if spec.arguments.len() > MAX_ARGUMENTS {
        return Err(ProcessError::InvalidSpec("too many arguments"));
    }
    if spec.environment.len() > MAX_ENVIRONMENT_ENTRIES {
        return Err(ProcessError::InvalidSpec("too many environment entries"));
    }
    let mut total = 0usize;
    for argument in &spec.arguments {
        if contains_nul(argument) {
            return Err(ProcessError::InvalidSpec("arguments must not contain NUL"));
        }
        total = add_size(total, argument.len())?;
    }
    for (index, (name, value)) in spec.environment.iter().enumerate() {
        validate_environment_name(name)?;
        if contains_nul(value) {
            return Err(ProcessError::InvalidSpec(
                "environment values must not contain NUL",
            ));
        }
        if spec.environment[..index]
            .iter()
            .any(|(earlier, _)| same_environment_name(earlier, name))
        {
            return Err(ProcessError::InvalidSpec("duplicate environment name"));
        }
        total = add_size(total, name.len())?;
        total = add_size(total, value.len())?;
    }
    platform_validate_spec(spec)
}

fn add_size(total: usize, more: usize) -> Result<usize, ProcessError> {
    total
        .checked_add(more)
        .filter(|sum| *sum <= MAX_COMMAND_BYTES)
        .ok_or(ProcessError::InvalidSpec(
            "arguments and environment are too large",
        ))
}

fn contains_nul(text: &OsStr) -> bool {
    text.as_encoded_bytes().contains(&0)
}

fn validate_environment_name(name: &OsStr) -> Result<(), ProcessError> {
    let bytes = name.as_encoded_bytes();
    if bytes.is_empty() || bytes.contains(&b'=') || bytes.contains(&0) {
        return Err(ProcessError::InvalidSpec(
            "environment names must be non-empty and contain no '=' or NUL",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn same_environment_name(first: &OsStr, second: &OsStr) -> bool {
    // Windows environment names compare case-insensitively.
    first.to_string_lossy().to_uppercase() == second.to_string_lossy().to_uppercase()
}

#[cfg(not(windows))]
fn same_environment_name(first: &OsStr, second: &OsStr) -> bool {
    first == second
}

#[cfg(windows)]
fn platform_validate_spec(spec: &ProcessSpec) -> Result<(), ProcessError> {
    windows::validate_spec(spec)
}

#[cfg(not(windows))]
fn platform_validate_spec(_spec: &ProcessSpec) -> Result<(), ProcessError> {
    Ok(())
}

/// Native control over one started tree.
trait ProcessTree {
    /// Return the program's termination if it has exited, without blocking.
    fn try_exit(&mut self) -> io::Result<Option<Termination>>;
    /// Ask every process in the tree to stop immediately.
    fn terminate(&mut self) -> io::Result<()>;
    /// Whether no process of the tree remains.
    fn is_empty(&mut self) -> io::Result<bool>;
    /// The tree limit reached so far, if any. Trees without limits have none.
    fn limit_reached(&mut self) -> io::Result<Option<ResourceLimit>> {
        Ok(None)
    }
    /// The tree limit reached by the time the program exited, taking any
    /// reports still queued. Called before the tree is stopped.
    fn limit_reached_at_exit(&mut self) -> io::Result<Option<ResourceLimit>> {
        self.limit_reached()
    }
    /// What a limited tree used. Called once, after the tree is empty.
    fn usage(&mut self) -> io::Result<Option<TreeUsage>> {
        Ok(None)
    }
    /// Release native limit state after the tree is observed empty.
    fn release(&mut self) -> Result<(), ProcessError> {
        Ok(())
    }
}

/// Capture threads for standard output and standard error.
struct Capture {
    stdout: Receiver<io::Result<CapturedOutput>>,
    stderr: Receiver<io::Result<CapturedOutput>>,
}

impl Capture {
    fn start(
        stdout: impl Read + Send + 'static,
        stderr: impl Read + Send + 'static,
        limits: ProcessLimits,
    ) -> io::Result<Self> {
        Ok(Self {
            stdout: spawn_reader("omgcode-stdout", stdout, limits.max_stdout_bytes)?,
            stderr: spawn_reader("omgcode-stderr", stderr, limits.max_stderr_bytes)?,
        })
    }

    /// Wait for both streams to close. A stream held open by a process outside
    /// the observed tree leaves its reader thread blocked and is an error.
    fn finish(self, deadline: Instant) -> Result<(CapturedOutput, CapturedOutput), ProcessError> {
        let stdout = receive(&self.stdout, deadline)?;
        let stderr = receive(&self.stderr, deadline)?;
        Ok((stdout, stderr))
    }
}

fn spawn_reader(
    name: &str,
    mut stream: impl Read + Send + 'static,
    limit: usize,
) -> io::Result<Receiver<io::Result<CapturedOutput>>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            // A send fails only when the supervisor stopped waiting; nothing remains to report.
            let _unreportable = sender.send(read_bounded(&mut stream, limit));
        })?;
    Ok(receiver)
}

/// Retain at most `limit` bytes, but keep draining so the writer never blocks.
fn read_bounded(stream: &mut impl Read, limit: usize) -> io::Result<CapturedOutput> {
    let mut captured = CapturedOutput::default();
    let mut buffer = [0u8; READ_CHUNK];
    loop {
        let read = match stream.read(&mut buffer) {
            Ok(0) => return Ok(captured),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            // Windows reports a closed pipe as broken rather than end of file.
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return Ok(captured),
            Err(error) => return Err(error),
        };
        let chunk = buffer.get(..read).unwrap_or_default();
        let room = limit.saturating_sub(captured.bytes.len());
        captured
            .bytes
            .extend_from_slice(chunk.get(..room.min(read)).unwrap_or_default());
        captured.total_bytes = captured
            .total_bytes
            .saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
    }
}

fn receive(
    receiver: &Receiver<io::Result<CapturedOutput>>,
    deadline: Instant,
) -> Result<CapturedOutput, ProcessError> {
    let wait = deadline.saturating_duration_since(Instant::now());
    match receiver.recv_timeout(wait) {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(ProcessError::Supervision(error)),
        Err(RecvTimeoutError::Timeout) => Err(ProcessError::CleanupUnconfirmed(
            "an output stream stayed open after the tree was empty",
        )),
        Err(RecvTimeoutError::Disconnected) => Err(ProcessError::Supervision(io::Error::other(
            "an output reader stopped without a result",
        ))),
    }
}

/// Supervise a started tree to completion and observe it empty.
fn supervise(
    tree: &mut impl ProcessTree,
    capture: Capture,
    started: Instant,
    limits: ProcessLimits,
    cancel: &CancelSignal,
    confinement: AppliedConfinement,
    mut disk: Option<DiskMonitor>,
) -> Result<ProcessOutcome, ProcessError> {
    let watched = watch(tree, started, limits.deadline, cancel, disk.as_ref());
    // Descendants never outlive the run, including after a normal exit.
    let stopped = stop_tree(tree, limits.cleanup_timeout);
    let mut termination = match (watched, stopped) {
        (Ok(termination), Ok(())) => termination,
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(ProcessError::Supervision(error)),
    };
    let elapsed = started.elapsed();
    // Release the native limit state even when usage cannot be read; a
    // failed release is unconfirmed cleanup and is reported first.
    let usage = tree.usage();
    tree.release()?;
    let mut usage = usage.map_err(ProcessError::Supervision)?;
    // The final sample runs after the tree is empty. It catches a program
    // that wrote past the limit and exited before the next scheduled sample.
    if let Some(disk) = &mut disk {
        disk.finish().map_err(ProcessError::Supervision)?;
        if disk.exceeded() && !matches!(termination, Termination::LimitExceeded { .. }) {
            termination = Termination::LimitExceeded {
                limit: ResourceLimit::Disk,
            };
        }
        if let Some(usage) = &mut usage {
            usage.peak_disk_bytes = Some(disk.peak_growth());
        }
    }
    let (stdout, stderr) = capture.finish(Instant::now() + limits.cleanup_timeout)?;
    Ok(ProcessOutcome {
        termination,
        confinement,
        stdout,
        stderr,
        elapsed,
        usage,
    })
}

fn watch(
    tree: &mut impl ProcessTree,
    started: Instant,
    deadline: Duration,
    cancel: &CancelSignal,
    disk: Option<&DiskMonitor>,
) -> io::Result<Termination> {
    loop {
        let exited = tree.try_exit()?;
        // A limit reached before or as the program exited ends the run as
        // limit exceeded: the exit may be the program's reaction to it.
        let reached = if exited.is_some() {
            tree.limit_reached_at_exit()?
        } else {
            tree.limit_reached()?
        };
        if let Some(limit) = reached {
            return Ok(Termination::LimitExceeded { limit });
        }
        // Disk growth is sampled off this thread; checking the result is
        // cheap. A measurement error fails the run rather than claiming a
        // limit that could not be checked.
        if let Some(disk) = disk
            && disk.check()?
        {
            return Ok(Termination::LimitExceeded {
                limit: ResourceLimit::Disk,
            });
        }
        if let Some(termination) = exited {
            return Ok(termination);
        }
        if cancel.is_cancelled() {
            return Ok(Termination::Cancelled);
        }
        if started.elapsed() >= deadline {
            return Ok(Termination::DeadlineExceeded);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Terminate the tree until it is observed empty or the timeout elapses.
fn stop_tree(tree: &mut impl ProcessTree, timeout: Duration) -> Result<(), ProcessError> {
    let until = Instant::now() + timeout;
    loop {
        tree.terminate().map_err(ProcessError::Supervision)?;
        if tree.is_empty().map_err(ProcessError::Supervision)? {
            return Ok(());
        }
        if Instant::now() >= until {
            return Err(ProcessError::CleanupUnconfirmed(
                "processes remained after termination",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Whether `path` names a Windows batch script, which `cmd.exe` would parse.
#[cfg_attr(not(windows), allow(dead_code))]
fn is_batch_script(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
        })
}
