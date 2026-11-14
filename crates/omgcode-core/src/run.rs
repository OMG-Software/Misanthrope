//! Trusted operator command runs behind the workspace `execute` preference.
//!
//! [`run`] checks the live settings, resolves the program through the
//! selected toolchain profile, records the intent in the run journal, runs the
//! process tree under native confinement, and records the outcome. Linux uses
//! Landlock and the seccomp socket filter; Windows uses the workspace
//! AppContainer and its recorded grants. Nothing falls back to an unconfined
//! run. No model tool calls this. See `docs/execution.md` and ADR-0030.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use omgcode_platform::broker::RelativePath;
use omgcode_platform::process::{
    self, AppliedConfinement, CapturedOutput, Confinement, NetworkAccess, ProcessError,
    ProcessOutcome, ProcessSpec, ResourceLimit, Termination, UnixSocketAccess,
};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::control::{ControlError, ControlStore, ProfilePlatform, ToolchainProfile};
use crate::policy::{PermissionSet, WorkspaceId};
use crate::run_journal::{
    ConfinementRecord, LimitKind, LimitsRecord, NetworkRecord, PendingRun, RunFinish, RunJournal,
    RunJournalError, RunResult, RunStart, SocketMode, UsageRecord, arguments_digest, describe,
};
use crate::session::{NativePath, SessionError};
use crate::storage::{self, StorageError};
use crate::workspace::DirectoryRecord;

pub use omgcode_platform::interrupt::{InterruptWatch, cancel_on_interrupt};
pub use omgcode_platform::process::{CancelSignal, ProcessLimits, TreeLimits};
pub use omgcode_platform::process::{
    MAX_CAPTURE_BYTES, MAX_DEADLINE, MAX_TREE_CPU_TIME, MAX_TREE_DISK_BYTES, MAX_TREE_MEMORY_BYTES,
    MAX_TREE_PROCESSES, MIN_TREE_DISK_BYTES, MIN_TREE_MEMORY_BYTES,
};

/// Default tree memory limit for a trusted run: 8 GiB.
pub const DEFAULT_TREE_MEMORY_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Default tree process limit for a trusted run. Linux counts threads too.
pub const DEFAULT_TREE_PROCESSES: u32 = 1024;
/// Default tree CPU-time limit for a trusted run: two CPU-hours.
pub const DEFAULT_TREE_CPU_TIME: Duration = Duration::from_secs(2 * 60 * 60);
/// Default tree disk limit for a trusted run: 8 GiB of growth.
pub const DEFAULT_TREE_DISK_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// How long to wait for another command's hold on the control journal.
const CONTROL_WAIT: Duration = Duration::from_secs(10);
const CONTROL_POLL: Duration = Duration::from_millis(20);
/// How often a run's watcher looks for a settings change.
const WATCH_POLL: Duration = Duration::from_millis(50);
/// How long a settings command waits for the runs it affects to stop.
pub const REVOCATION_WAIT: Duration = Duration::from_secs(60);
/// Directory of per-run owner locks inside the run journal's directory.
const OWNER_DIRECTORY: &str = "run-owners";
/// Linux devices a run may read. `/dev/null` is also writable.
#[cfg(target_os = "linux")]
const READ_ONLY_DEVICES: [&str; 3] = ["/dev/zero", "/dev/random", "/dev/urandom"];

/// One requested run. Paths and names here are operator input, not grants.
#[derive(Clone, Debug)]
pub struct RunRequest {
    /// The registered workspace to run in.
    pub workspace_id: WorkspaceId,
    /// The recorded toolchain profile name.
    pub profile: String,
    /// An absolute path, a bare name searched in the profile's `PATH`
    /// entries, or a workspace-relative path containing a separator.
    pub program: PathBuf,
    /// Arguments after the program, passed exactly.
    pub arguments: Vec<OsString>,
    /// Workspace-relative working directory; the workspace when `None`.
    pub working_directory: Option<PathBuf>,
    /// Ask for host-service Unix-socket access. Needs the workspace
    /// `host_services` preference; refused on Windows.
    pub host_services: bool,
    /// Ask for outbound IP network access. Needs the workspace
    /// `task_network` preference. On Linux the run can then reach any address,
    /// loopback included; on Windows only the internet. See ADR-0032.
    pub network: bool,
    /// Deadline, output, cleanup, and tree bounds. Tree limits are required;
    /// a request without them is refused before anything is recorded.
    pub limits: ProcessLimits,
}

/// Output returned to the caller. It is never written to a journal.
#[derive(Clone, Debug, Serialize)]
pub struct CapturedText {
    /// The retained bytes as UTF-8, with invalid sequences replaced.
    pub text: String,
    /// Whether the retained bytes were valid UTF-8.
    pub utf8: bool,
    /// Every byte the tree wrote to this stream.
    pub total_bytes: u64,
    /// Whether bytes beyond the retention limit were discarded.
    pub truncated: bool,
}

impl CapturedText {
    fn from_output(output: &CapturedOutput) -> Self {
        Self {
            text: String::from_utf8_lossy(&output.bytes).into_owned(),
            utf8: std::str::from_utf8(&output.bytes).is_ok(),
            total_bytes: output.total_bytes,
            truncated: output.truncated(),
        }
    }
}

/// The result of a supervised run whose outcome was recorded.
#[derive(Clone, Debug, Serialize)]
pub struct RunReport {
    /// The recorded intent: profile, program, argument digest, and mode.
    pub start: RunStart,
    /// The recorded outcome.
    pub finish: RunFinish,
    /// Standard output.
    pub stdout: CapturedText,
    /// Standard error.
    pub stderr: CapturedText,
    /// Whether the Linux per-run scratch directory was removed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scratch_removed: Option<bool>,
    /// Why a settings change stopped the run, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_because: Option<&'static str>,
}

/// Runs that a settings change no longer allowed.
#[derive(Debug, Default, Serialize)]
pub struct WithdrawnRuns {
    /// Runs that stopped and whose outcome was recorded.
    pub stopped: Vec<Uuid>,
    /// Runs not observed to stop: their supervisor is still live, cleanup
    /// was unconfirmed, or the supervisor ended without an outcome, including
    /// a run acknowledged without one.
    pub unconfirmed: Vec<Uuid>,
}

/// Recent run records for trusted inspection.
#[derive(Debug, Serialize)]
pub struct RunAuditView {
    /// Runs whose outcome is missing or needs acknowledgement.
    pub pending: Vec<PendingRun>,
    /// Validated journal events, oldest first.
    pub entries: Vec<serde_json::Value>,
}

/// Receipt for an archived run journal.
#[derive(Debug, Serialize)]
pub struct RunsArchived {
    /// Lossless path of the archived journal.
    pub archive: NativePath,
    /// SHA-256 of the exact archived bytes.
    pub sha256: String,
    /// Number of archived events, including the header.
    pub event_count: usize,
}

/// A run failure. Variants with a `run_id` were recorded in the run journal.
#[derive(Debug, Error)]
pub enum RunError {
    /// Settings could not be read or failed a check.
    #[error("workspace control failed: {0}")]
    Control(#[from] ControlError),
    /// The run journal could not be opened or written.
    #[error("run journal failed: {0}")]
    Journal(#[from] RunJournalError),
    /// The settings do not allow this run. Nothing was recorded.
    #[error("run refused: {0}")]
    Refused(&'static str),
    /// The run's supervising process is still live, so its record cannot be
    /// acknowledged yet.
    #[error("run {0} is still live; stop it before acknowledging it")]
    Live(Uuid),
    /// A profile directory was moved, replaced, or removed. Nothing was
    /// recorded. Remove the profile and add it again.
    #[error("toolchain profile is stale: {0}")]
    StaleProfile(&'static str),
    /// The program could not be resolved inside the allowed directories.
    #[error("program refused: {0}")]
    Program(&'static str),
    /// A path could not be checked or encoded.
    #[error("run preparation failed: {0}")]
    Io(#[from] io::Error),
    /// A limit is out of range. Nothing was recorded.
    #[error("run limits are out of range: {0}")]
    InvalidLimits(#[source] ProcessError),
    /// Tree limits cannot be applied on this host, for example because a
    /// Linux runner is not in a delegated cgroup. Nothing was recorded.
    #[error("tree limits cannot be applied: {0}")]
    Limits(#[source] ProcessError),
    /// The intent was recorded, but nothing started. The outcome is recorded.
    #[error("run {run_id} did not start: {source}")]
    NotStarted {
        run_id: Uuid,
        #[source]
        source: ProcessError,
    },
    /// The program started, but supervision failed. The tree was observed
    /// empty and the outcome is recorded.
    #[error("run {run_id} failed during supervision: {source}")]
    Supervision {
        run_id: Uuid,
        #[source]
        source: ProcessError,
    },
    /// The tree was not observed empty; processes may remain. The run stays
    /// pending until it is acknowledged.
    #[error("run {run_id} cleanup was not confirmed; processes may remain: {source}")]
    CleanupUnconfirmed {
        run_id: Uuid,
        #[source]
        source: ProcessError,
    },
    /// The run ended, but its outcome could not be recorded. It stays
    /// pending. The report is returned so the output is not lost.
    #[error("run {} ended, but its outcome could not be recorded: {source}", report.start.run_id)]
    OutcomeNotRecorded {
        report: Box<RunReport>,
        #[source]
        source: RunJournalError,
    },
}

impl From<StorageError> for RunError {
    fn from(error: StorageError) -> Self {
        Self::Journal(error.into())
    }
}

impl From<SessionError> for RunError {
    fn from(error: SessionError) -> Self {
        Self::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            error.to_string(),
        ))
    }
}

/// Everything a run needs, checked while the control journal was locked.
struct Prepared {
    start: RunStart,
    spec: ProcessSpec,
    confinement: Confinement,
    scratch: Option<Scratch>,
}

/// The Linux per-run scratch directory, removed after the run.
struct Scratch {
    path: PathBuf,
    removed: bool,
}

impl Scratch {
    /// Remove the directory tree without following links inside it.
    fn remove(&mut self) -> bool {
        self.removed = fs::remove_dir_all(&self.path).is_ok();
        self.removed
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if !self.removed {
            // Best effort on an early return; failures are reported only by
            // the explicit `remove` on the normal path.
            self.remove();
        }
    }
}

/// The lock a run's supervising process holds from before its intent is
/// recorded until its outcome is recorded. Other commands use it to tell a
/// live run from one whose supervisor ended.
struct OwnerLock {
    file: Option<File>,
    path: PathBuf,
}

impl OwnerLock {
    fn acquire(journal_root: &Path, run_id: Uuid) -> Result<Self, RunError> {
        storage::create_private_directories(&journal_root.join(OWNER_DIRECTORY))?;
        let path = owner_path(journal_root, run_id);
        let file = storage::open_locked_file(&path, true)?;
        Ok(Self {
            file: Some(file),
            path,
        })
    }

    /// Release the lock, then remove its file. A file left behind is
    /// unlocked, so it never marks a run as live.
    fn release(mut self) -> io::Result<()> {
        drop(self.file.take());
        fs::remove_file(&self.path)
    }
}

impl Drop for OwnerLock {
    fn drop(&mut self) {
        // Fallback on an early return: closing the handle releases the lock,
        // and removal is best effort for the reason given on `release`.
        if self.file.take().is_some() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn owner_path(journal_root: &Path, run_id: Uuid) -> PathBuf {
    journal_root
        .join(OWNER_DIRECTORY)
        .join(format!("{run_id}.lock"))
}

/// The state of a run's supervising process.
enum Owner {
    /// It still holds its lock.
    Live,
    /// It released its lock, which is now held here, or left no lock file.
    Ended(Option<File>),
}

fn probe_owner(journal_root: &Path, run_id: Uuid) -> Result<Owner, RunError> {
    match storage::open_locked_file(&owner_path(journal_root, run_id), false) {
        Ok(file) => Ok(Owner::Ended(Some(file))),
        Err(StorageError::Busy) => Ok(Owner::Live),
        Err(StorageError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            Ok(Owner::Ended(None))
        }
        Err(error) => Err(error.into()),
    }
}

/// Check, record, run, and record the outcome of one command.
///
/// The control and run journal locks are held only while checking and while
/// appending, never while the program runs, so concurrent runs and settings
/// commands can proceed. While the program runs, a watcher rereads the
/// settings after each change. When they no longer allow the run, it stops
/// the whole tree and the outcome is `revoked`. `cancel` terminates the whole
/// tree; the outcome is then `cancelled`.
pub fn run(
    data_root: &Path,
    request: &RunRequest,
    cancel: &CancelSignal,
) -> Result<RunReport, RunError> {
    let platform = ProfilePlatform::current().ok_or(RunError::Refused(
        "trusted runs exist only on Linux and Windows",
    ))?;
    let control_path = ControlStore::journal_path(data_root);
    let (journal_root, store_id, mut prepared, owner, baseline, revocations) = {
        let control = open_control(data_root)?;
        let (journal_root, store_id) = control.file_audit_context()?;
        let prepared = prepare(&control, request, platform, &journal_root)?;
        // Taken under the control lock, so every later change is seen.
        let baseline = stamp(&control_path)?;
        let revocations = control.grant_revocations(request.workspace_id);
        let owner = OwnerLock::acquire(&journal_root, prepared.start.run_id)?;
        let mut journal = RunJournal::open(&journal_root, store_id)?;
        journal.start(prepared.start.clone())?;
        (
            journal_root,
            store_id,
            prepared,
            owner,
            baseline,
            revocations,
        )
    };
    let run_id = prepared.start.run_id;
    let stop = CancelSignal::new();
    let done = AtomicBool::new(false);
    let revoked = OnceLock::new();
    let result = thread::scope(|scope| {
        let watch = Watch {
            data_root,
            control_path: &control_path,
            start: &prepared.start,
            revocations,
            caller: cancel,
            stop: &stop,
            done: &done,
            revoked: &revoked,
        };
        scope.spawn(move || watch.run(baseline));
        let result = process::run_confined(
            &prepared.spec,
            &prepared.confinement,
            &request.limits,
            &stop,
        );
        done.store(true, Ordering::SeqCst);
        result
    });
    let revoked_because = revoked.get().copied();
    let scratch_removed = prepared.scratch.as_mut().map(Scratch::remove);
    let (finish, outcome, failure) = settle(run_id, result, revoked_because.is_some());
    let report = RunReport {
        start: prepared.start,
        finish,
        stdout: CapturedText::from_output(
            &outcome
                .as_ref()
                .map(|o| o.stdout.clone())
                .unwrap_or_default(),
        ),
        stderr: CapturedText::from_output(
            &outcome
                .as_ref()
                .map(|o| o.stderr.clone())
                .unwrap_or_default(),
        ),
        scratch_removed,
        revoked_because,
    };
    let recorded = RunJournal::open(&journal_root, store_id)
        .and_then(|mut journal| journal.finish(run_id, finish));
    // Only after the outcome is recorded may other commands treat the run
    // as ended. A failed removal leaves an unlocked file, which is harmless.
    let _ = owner.release();
    if let Err(source) = recorded {
        return Err(RunError::OutcomeNotRecorded {
            report: Box::new(report),
            source,
        });
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(report),
    }
}

/// The size and modification time of the control journal.
type Stamp = (u64, Option<SystemTime>);

fn stamp(path: &Path) -> io::Result<Stamp> {
    let metadata = fs::symlink_metadata(path)?;
    Ok((metadata.len(), metadata.modified().ok()))
}

/// Watches one run: forwards the caller's cancellation, and stops the run
/// when the recorded settings no longer allow it.
struct Watch<'a> {
    data_root: &'a Path,
    control_path: &'a Path,
    start: &'a RunStart,
    /// Container revocations of the workspace recorded when the run started.
    revocations: u64,
    caller: &'a CancelSignal,
    stop: &'a CancelSignal,
    done: &'a AtomicBool,
    revoked: &'a OnceLock<&'static str>,
}

impl Watch<'_> {
    fn run(&self, mut seen: Stamp) {
        while !self.done.load(Ordering::SeqCst) {
            if self.caller.is_cancelled() {
                self.stop.cancel();
            }
            if let Some(reason) = self.recheck(&mut seen) {
                let _ = self.revoked.set(reason);
                self.stop.cancel();
                return;
            }
            thread::sleep(WATCH_POLL);
        }
    }

    /// Reread the settings when the journal changed. Settings that cannot be
    /// read withdraw the run; another command's lock is retried next poll.
    fn recheck(&self, seen: &mut Stamp) -> Option<&'static str> {
        let Ok(current) = stamp(self.control_path) else {
            return Some("the settings could not be read");
        };
        if current == *seen {
            return None;
        }
        match ControlStore::open_existing(self.data_root) {
            Ok(Some(control)) => {
                *seen = current;
                withdrawn(&control, self.start).or_else(|| {
                    // A grant can be added and revoked between two polls, so
                    // its state alone can miss a revocation; the count cannot.
                    (control.grant_revocations(self.start.workspace_id) != self.revocations)
                        .then_some("a container grant of this workspace was revoked")
                })
            }
            Ok(None) => Some("the settings were removed"),
            Err(ControlError::Busy) => None,
            Err(_) => Some("the settings could not be read"),
        }
    }
}

/// Why the recorded settings no longer allow a started run, if they don't.
/// Settings commands and run watchers use this same rule.
fn withdrawn(control: &ControlStore, start: &RunStart) -> Option<&'static str> {
    let Some(registration) = control.workspace(start.workspace_id) else {
        return Some("the workspace is no longer registered");
    };
    if let Err(reason) = permitted(
        registration.permissions(),
        start.unix_sockets == SocketMode::HostServices,
        start.network.is_some(),
    ) {
        return Some(reason);
    }
    match control.profile(&start.profile_name) {
        Some(status) if status.profile().id() == start.profile_id => {}
        _ => return Some("the toolchain profile was removed"),
    }
    let Ok(scope) = control.file_scope(start.workspace_id) else {
        return Some("the workspace is outside the development root");
    };
    workspace_access(control, start.workspace_id, &scope.workspace, true).err()
}

/// The preferences a run needs for its whole life. A run writes inside the
/// workspace, so it needs `read` and `write` as well as `execute`.
fn permitted(
    permissions: PermissionSet,
    host_services: bool,
    network: bool,
) -> Result<(), &'static str> {
    if !permissions.execute {
        return Err("the workspace execute preference is disabled");
    }
    if !permissions.read || !permissions.write {
        return Err("the workspace read and write preferences must both be enabled");
    }
    if host_services && !permissions.host_services {
        return Err("the workspace host_services preference is disabled");
    }
    if network && !permissions.task_network {
        return Err("the workspace task_network preference is disabled");
    }
    Ok(())
}

/// The workspace must hold an active read-write grant. Before a run starts,
/// no grant of the workspace may be pending or being revoked. During a run,
/// a pending grant only adds access, so only a revocation withdraws it.
#[cfg(windows)]
fn workspace_access(
    control: &ControlStore,
    workspace_id: WorkspaceId,
    workspace: &DirectoryRecord,
    running: bool,
) -> Result<(), &'static str> {
    use crate::control::{ContainerGrantAccess, ContainerGrantState};

    let grants: Vec<_> = control
        .container_grants()
        .iter()
        .filter(|status| status.grant().workspace_id() == workspace_id)
        .collect();
    if grants.iter().any(|status| match status.state() {
        ContainerGrantState::Active => false,
        ContainerGrantState::Pending => !running,
        ContainerGrantState::Revoking => true,
    }) {
        return Err(if running {
            "a container grant of this workspace is being revoked"
        } else {
            "a container grant of this workspace is pending or being revoked; revoke it first"
        });
    }
    if !grants.iter().any(|status| {
        status.state() == ContainerGrantState::Active
            && status.grant().access() == ContainerGrantAccess::ReadWrite
            && status.grant().directory().same_directory(workspace)
    }) {
        return Err(if running {
            "the workspace read-write container grant was revoked"
        } else {
            "grant the workspace directory read-write to its container first"
        });
    }
    Ok(())
}

#[cfg(not(windows))]
#[allow(clippy::unnecessary_wraps)]
fn workspace_access(
    _control: &ControlStore,
    _workspace_id: WorkspaceId,
    _workspace: &DirectoryRecord,
    _running: bool,
) -> Result<(), &'static str> {
    Ok(())
}

/// The pending runs of one workspace: runs with no outcome yet and runs
/// whose cleanup was unconfirmed.
///
/// `workspace container-revoke` calls this while it holds the control lock,
/// before it records the revocation. Runs start only under that lock, so the
/// result is exactly the runs that started before the revocation, which it
/// then passes to [`stop_withdrawn_runs`].
pub fn workspace_runs(
    control: &ControlStore,
    workspace_id: WorkspaceId,
) -> Result<Vec<Uuid>, RunError> {
    let (journal_root, store_id) = control.file_audit_context()?;
    let Some(journal) = RunJournal::open_existing(&journal_root, store_id)? else {
        return Ok(Vec::new());
    };
    Ok(journal
        .pending_starts()
        .into_iter()
        .filter(|start| start.workspace_id == workspace_id)
        .map(|start| start.run_id)
        .collect())
}

/// Wait for every pending run that the recorded settings no longer allow,
/// and every pending run in `also_withdrawn`, to stop and record its outcome.
///
/// Settings commands that can withdraw access call this after their change
/// is recorded and their control lock is released, so each run's watcher can
/// read the change. A run counts as stopped only when its supervisor released
/// its owner lock and an outcome other than `cleanup_unconfirmed` is recorded.
/// A run whose cleanup was unconfirmed, or which was acknowledged without an
/// outcome, may still have processes, so it is reported as unconfirmed, as is
/// a run still live when `wait` ends. The caller must not report the change
/// as complete while any run is unconfirmed.
pub fn stop_withdrawn_runs(
    data_root: &Path,
    wait: Duration,
    also_withdrawn: &[Uuid],
) -> Result<WithdrawnRuns, RunError> {
    let (journal_root, store_id, affected) = {
        let control = open_control(data_root)?;
        let (journal_root, store_id) = control.file_audit_context()?;
        let Some(journal) = RunJournal::open_existing(&journal_root, store_id)? else {
            return Ok(WithdrawnRuns::default());
        };
        let affected: Vec<Uuid> = journal
            .pending_starts()
            .into_iter()
            .filter(|start| {
                also_withdrawn.contains(&start.run_id) || withdrawn(&control, start).is_some()
            })
            .map(|start| start.run_id)
            .collect();
        (journal_root, store_id, affected)
    };
    if affected.is_empty() {
        return Ok(WithdrawnRuns::default());
    }
    let until = Instant::now() + wait;
    for run_id in &affected {
        // A run still live at the deadline stays pending and is reported below.
        while matches!(probe_owner(&journal_root, *run_id)?, Owner::Live) && Instant::now() < until
        {
            thread::sleep(CONTROL_POLL);
        }
    }
    let journal = RunJournal::open(&journal_root, store_id)?;
    confirm_stopped(&journal_root, &journal, affected)
}

/// Split withdrawn runs into those observed to stop and the rest. The owner
/// lock is probed again after the journal is read, so a run whose supervisor
/// is still live is never counted as stopped.
fn confirm_stopped(
    journal_root: &Path,
    journal: &RunJournal,
    affected: Vec<Uuid>,
) -> Result<WithdrawnRuns, RunError> {
    let mut runs = WithdrawnRuns::default();
    for run_id in affected {
        let recorded = matches!(
            journal.outcome(run_id),
            Some(result) if result != RunResult::CleanupUnconfirmed
        );
        if recorded && matches!(probe_owner(journal_root, run_id)?, Owner::Ended(_)) {
            runs.stopped.push(run_id);
        } else {
            runs.unconfirmed.push(run_id);
        }
    }
    Ok(runs)
}

/// Map the runner's result to a recorded outcome and, for failures, an error.
/// A tree stopped because access was withdrawn is recorded as `revoked`.
fn settle(
    run_id: Uuid,
    result: Result<ProcessOutcome, ProcessError>,
    revoked: bool,
) -> (RunFinish, Option<ProcessOutcome>, Option<RunError>) {
    match result {
        Ok(outcome) => {
            let finish = RunFinish {
                result: match outcome.termination {
                    Termination::Exited { code } => RunResult::Exited { code },
                    Termination::Signalled { signal } => RunResult::Signalled { signal },
                    Termination::DeadlineExceeded => RunResult::DeadlineExceeded,
                    Termination::Cancelled if revoked => RunResult::Revoked,
                    Termination::Cancelled => RunResult::Cancelled,
                    Termination::LimitExceeded { limit } => RunResult::LimitExceeded {
                        limit: match limit {
                            ResourceLimit::Memory => LimitKind::Memory,
                            ResourceLimit::Processes => LimitKind::Processes,
                            ResourceLimit::CpuTime => LimitKind::CpuTime,
                            ResourceLimit::Disk => LimitKind::Disk,
                        },
                    },
                },
                confinement: Some(record_confinement(outcome.confinement)),
                elapsed_ms: millis(outcome.elapsed),
                stdout_bytes: outcome.stdout.total_bytes,
                stderr_bytes: outcome.stderr.total_bytes,
                usage: outcome.usage.map(|usage| UsageRecord {
                    cpu_ms: millis(usage.cpu_time),
                    peak_memory_bytes: usage.peak_memory_bytes,
                    peak_disk_bytes: usage.peak_disk_bytes,
                }),
            };
            (finish, Some(outcome), None)
        }
        Err(source) => {
            let (result, error) = match source {
                ProcessError::Supervision(_) => (
                    RunResult::SupervisionFailed,
                    RunError::Supervision { run_id, source },
                ),
                ProcessError::CleanupUnconfirmed(_) => (
                    RunResult::CleanupUnconfirmed,
                    RunError::CleanupUnconfirmed { run_id, source },
                ),
                source => (
                    RunResult::NotStarted,
                    RunError::NotStarted { run_id, source },
                ),
            };
            let finish = RunFinish {
                result,
                confinement: (result != RunResult::NotStarted).then_some(platform_confinement()),
                elapsed_ms: 0,
                stdout_bytes: 0,
                stderr_bytes: 0,
                usage: None,
            };
            (finish, None, Some(error))
        }
    }
}

/// The confinement a started run had when the runner reported no outcome.
/// Linux reports its ABI only in an outcome, so a failed run records ABI 0.
fn platform_confinement() -> ConfinementRecord {
    if cfg!(windows) {
        ConfinementRecord::AppContainer
    } else {
        ConfinementRecord::Landlock { abi: 0 }
    }
}

fn record_confinement(applied: AppliedConfinement) -> ConfinementRecord {
    match applied {
        AppliedConfinement::Landlock { abi, .. } => ConfinementRecord::Landlock { abi },
        // `run_confined` never reports an unconfined run; record the
        // container either way rather than claim less than applied.
        AppliedConfinement::AppContainer { .. } | AppliedConfinement::Unconfined => {
            ConfinementRecord::AppContainer
        }
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Run every check that needs the settings, and build the spec, the
/// confinement, and the intent record.
fn prepare(
    control: &ControlStore,
    request: &RunRequest,
    platform: ProfilePlatform,
    data_root: &Path,
) -> Result<Prepared, RunError> {
    // The platform runner checks these again, but only after the intent is
    // recorded; check them here so an out-of-range value records nothing.
    let limits = request.limits.validate().map_err(RunError::InvalidLimits)?;
    let tree = limits.tree.ok_or(RunError::Refused(
        "a trusted run needs tree limits; none were given",
    ))?;
    let disk = tree.max_disk_bytes.ok_or(RunError::Refused(
        "a trusted run needs a disk limit; none was given",
    ))?;
    let registration = control
        .workspace(request.workspace_id)
        .ok_or(ControlError::UnknownWorkspace)?;
    let permissions = registration.permissions();
    let scope = control.file_scope(request.workspace_id)?;
    if request.host_services && platform == ProfilePlatform::Windows {
        return Err(RunError::Refused(
            "host-service access is not available on Windows",
        ));
    }
    permitted(permissions, request.host_services, request.network).map_err(RunError::Refused)?;
    let status = control
        .profile(&request.profile)
        .ok_or(ControlError::UnknownProfile)?;
    let profile = status.profile();
    if profile.platform() != platform {
        return Err(RunError::Refused(
            "the profile was declared on another platform",
        ));
    }
    let roots = current_roots(control, profile)?;
    // On Linux this needs a delegated cgroup; without one nothing is recorded.
    process::check_tree_limits().map_err(RunError::Limits)?;
    let workspace = native_form(scope.workspace.live_path()?)?;
    let working_directory = working_directory(&scope.workspace, &workspace, request)?;
    let path_entries = profile
        .path_entries()
        .iter()
        .map(|entry| {
            if entry.is_current()? {
                native_form(entry.live_path()?)
            } else {
                Err(RunError::StaleProfile(
                    "a PATH entry was moved, replaced, or removed",
                ))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let program = resolve_program(
        &request.program,
        &path_entries,
        profile.read_only_roots(),
        &scope.workspace,
        &workspace,
    )?;
    let run_id = Uuid::new_v4();
    let search_path = std::env::join_paths(&path_entries)
        .map_err(|_| RunError::Refused("a PATH entry contains the platform's list separator"))?;
    let mut environment = vec![(OsString::from("PATH"), search_path)];
    let (confinement, scratch, support_paths, package_cache) = platform_setup(
        control,
        request,
        &scope.workspace,
        &workspace,
        roots,
        data_root,
        run_id,
        &mut environment,
    )?;
    for variable in profile.environment() {
        environment.push((variable.name().into(), variable.value().into()));
    }
    let start = RunStart {
        run_id,
        workspace_id: request.workspace_id,
        settings_revision: control.revision(),
        profile_id: profile.id(),
        profile_name: profile.name().to_owned(),
        profile_revision: status.revision(),
        program: NativePath::from_path(&program)?,
        argument_count: request.arguments.len(),
        arguments_sha256: arguments_digest(&request.arguments),
        working_directory: NativePath::from_path(&working_directory)?,
        unix_sockets: if request.host_services {
            SocketMode::HostServices
        } else {
            SocketMode::PairsOnly
        },
        limits: Some(LimitsRecord {
            max_memory_bytes: tree.max_memory_bytes,
            max_processes: tree.max_processes,
            max_cpu_ms: millis(tree.max_cpu_time),
            max_disk_bytes: Some(disk),
        }),
        network: if request.network {
            Some(NetworkRecord {
                support_paths: support_paths
                    .iter()
                    .map(|path| NativePath::from_path(path))
                    .collect::<Result<_, _>>()?,
            })
        } else {
            None
        },
        package_cache,
    };
    Ok(Prepared {
        start,
        spec: ProcessSpec {
            program,
            arguments: request.arguments.clone(),
            working_directory,
            environment,
        },
        confinement,
        scratch,
    })
}

/// Recheck each profile root and return its live path.
fn current_roots(
    control: &ControlStore,
    profile: &ToolchainProfile,
) -> Result<Vec<PathBuf>, RunError> {
    profile
        .read_only_roots()
        .iter()
        .map(|root| {
            if !root.is_current()? {
                return Err(RunError::StaleProfile(
                    "a read-only root was moved, replaced, or removed",
                ));
            }
            control.check_toolchain_root(root)?;
            native_form(root.live_path()?)
        })
        .collect()
}

/// Resolve the working directory inside the workspace.
fn working_directory(
    record: &DirectoryRecord,
    workspace: &Path,
    request: &RunRequest,
) -> Result<PathBuf, RunError> {
    let Some(relative) = &request.working_directory else {
        return Ok(workspace.to_path_buf());
    };
    RelativePath::new(relative)
        .map_err(|_| RunError::Refused("the working directory must be workspace-relative"))?;
    let directory = DirectoryRecord::capture(&workspace.join(relative))?;
    if !record.contains(&directory)? {
        return Err(RunError::Refused(
            "the working directory must stay inside the workspace",
        ));
    }
    native_form(directory.live_path()?)
}

/// Find the program and require it to lie inside a profile root or the
/// workspace. The path that runs is the one found, before links resolve.
fn resolve_program(
    program: &Path,
    path_entries: &[PathBuf],
    roots: &[DirectoryRecord],
    workspace_record: &DirectoryRecord,
    workspace: &Path,
) -> Result<PathBuf, RunError> {
    let mut components = program.components();
    let bare = matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    );
    let candidate = if bare {
        search(program, path_entries).ok_or(RunError::Program(
            "no profile PATH entry contains this program",
        ))?
    } else if program.is_absolute() {
        program.to_path_buf()
    } else {
        RelativePath::new(program).map_err(|_| {
            RunError::Program("a relative program path must stay inside the workspace")
        })?;
        workspace.join(program)
    };
    let resolved = fs::canonicalize(&candidate)
        .map_err(|_| RunError::Program("the program does not exist"))?;
    if !fs::metadata(&resolved).is_ok_and(|metadata| metadata.is_file()) {
        return Err(RunError::Program("the program is not a regular file"));
    }
    let parent = resolved
        .parent()
        .ok_or(RunError::Program("the program has no parent directory"))?;
    let directory = DirectoryRecord::capture(parent)?;
    let mut allowed = workspace_record.contains(&directory)?;
    for root in roots {
        allowed = allowed || root.contains(&directory)?;
    }
    if !allowed {
        return Err(RunError::Program(
            "the program must lie inside a profile root or the workspace",
        ));
    }
    native_form(candidate)
}

/// Search the profile's `PATH` entries, never the caller's. On Windows a
/// name without an extension also matches `<name>.exe`.
fn search(name: &Path, path_entries: &[PathBuf]) -> Option<PathBuf> {
    for entry in path_entries {
        let mut names = vec![entry.join(name)];
        if cfg!(windows) && name.extension().is_none() {
            names.insert(0, entry.join(name).with_extension("exe"));
        }
        if let Some(found) = names.into_iter().find(|path| path.is_file()) {
            return Some(found);
        }
    }
    None
}

/// The platform network mode a request selects.
#[cfg(any(target_os = "linux", windows))]
fn network_access(request: &RunRequest) -> NetworkAccess {
    if request.network {
        NetworkAccess::Internet
    } else {
        NetworkAccess::Denied
    }
}

/// Build the platform confinement and the runner-owned variables. Also
/// return the resolver and certificate paths added for network access.
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn platform_setup(
    control: &ControlStore,
    request: &RunRequest,
    _workspace_record: &DirectoryRecord,
    workspace: &Path,
    roots: Vec<PathBuf>,
    data_root: &Path,
    run_id: Uuid,
    environment: &mut Vec<(OsString, OsString)>,
) -> Result<(Confinement, Option<Scratch>, Vec<PathBuf>, bool), RunError> {
    let parent = data_root.join("run-scratch");
    crate::storage::create_private_directories(&parent)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let path = parent.join(run_id.to_string());
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(&path)?;
    }
    let scratch = Scratch {
        path: path.clone(),
        removed: false,
    };
    for name in ["HOME", "TMPDIR", "XDG_CACHE_HOME"] {
        environment.push((name.into(), path.clone().into_os_string()));
    }
    let mut read_only = roots;
    read_only.extend(
        READ_ONLY_DEVICES
            .iter()
            .map(PathBuf::from)
            .filter(|device| device.exists()),
    );
    let support_paths = if request.network {
        process::network_support_paths()?
    } else {
        Vec::new()
    };
    read_only.extend(support_paths.iter().cloned());
    // Expose only this workspace's fetched-package cache. Application storage
    // otherwise stays outside the run's view.
    let package_cache = package_cache_path(control, data_root, request.workspace_id);
    if let Some(cache) = &package_cache {
        read_only.push(cache.clone());
        environment.push(("OMGCODE_PACKAGE_CACHE".into(), cache.as_os_str().to_owned()));
    }
    let confinement = Confinement {
        read_only,
        read_write: vec![workspace.to_path_buf(), path, PathBuf::from("/dev/null")],
        unix_sockets: if request.host_services {
            UnixSocketAccess::HostServices
        } else {
            UnixSocketAccess::PairsOnly
        },
        network: network_access(request),
        container: None,
    };
    Ok((
        confinement,
        Some(scratch),
        support_paths,
        package_cache.is_some(),
    ))
}

/// The private package cache for one workspace, when it exists as a plain
/// directory. A configured root outside application storage takes precedence.
/// Linux falls back to its application-storage cache; Windows has no fallback.
/// A link or other entry is not exposed.
fn package_cache_path(
    control: &ControlStore,
    data_root: &Path,
    workspace_id: WorkspaceId,
) -> Option<PathBuf> {
    let path = match control.package_cache_root(workspace_id) {
        Some(root) => {
            if !root.is_current().ok()? {
                return None;
            }
            root.live_path().ok()?
        }
        None => {
            #[cfg(target_os = "linux")]
            {
                data_root
                    .join("package-cache")
                    .join(workspace_id.to_string())
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = data_root;
                return None;
            }
        }
    };
    // Normalize a recorded Windows path to the drive-letter form tools accept.
    let path = native_form(path).ok()?;
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Some(path),
        _ => None,
    }
}

/// Build the platform confinement and the runner-owned variables. Windows
/// adds no support paths for network access: name resolution and
/// certificates come from system services.
///
/// The workspace must hold an active read-write grant, and no grant of the
/// workspace may be pending or being revoked. Profile roots are checked
/// against their live ACLs by the runner; roots that every application
/// package can read need no grant.
#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn platform_setup(
    control: &ControlStore,
    request: &RunRequest,
    workspace_record: &DirectoryRecord,
    workspace: &Path,
    roots: Vec<PathBuf>,
    data_root: &Path,
    _run_id: Uuid,
    environment: &mut Vec<(OsString, OsString)>,
) -> Result<(Confinement, Option<Scratch>, Vec<PathBuf>, bool), RunError> {
    use crate::control::container_name;

    workspace_access(control, request.workspace_id, workspace_record, false)
        .map_err(RunError::Refused)?;
    let name = container_name(request.workspace_id)?;
    let home = native_form(omgcode_platform::container::profile_folder(&name)?)?;
    let local = std::env::var_os("LOCALAPPDATA").ok_or(RunError::Refused(
        "LOCALAPPDATA must be set to start a container process",
    ))?;
    environment.push(("LOCALAPPDATA".into(), local));
    for name in ["SystemRoot", "windir"] {
        if let Some(value) = std::env::var_os(name) {
            environment.push((name.into(), value));
        }
    }
    environment.push(("HOME".into(), home.clone().into_os_string()));
    environment.push(("USERPROFILE".into(), home.into_os_string()));
    let package_cache = package_cache_path(control, data_root, request.workspace_id);
    let mut read_only = roots;
    if let Some(cache) = &package_cache {
        read_only.push(cache.clone());
        environment.push(("OMGCODE_PACKAGE_CACHE".into(), cache.as_os_str().to_owned()));
    }
    let confinement = Confinement {
        read_only,
        read_write: vec![workspace.to_path_buf()],
        unix_sockets: UnixSocketAccess::PairsOnly,
        network: network_access(request),
        container: Some(name),
    };
    Ok((confinement, None, Vec::new(), package_cache.is_some()))
}

#[cfg(not(any(target_os = "linux", windows)))]
#[allow(clippy::too_many_arguments)]
fn platform_setup(
    _control: &ControlStore,
    _request: &RunRequest,
    _workspace_record: &DirectoryRecord,
    _workspace: &Path,
    _roots: Vec<PathBuf>,
    _data_root: &Path,
    _run_id: Uuid,
    _environment: &mut Vec<(OsString, OsString)>,
) -> Result<(Confinement, Option<Scratch>, Vec<PathBuf>, bool), RunError> {
    Err(RunError::Refused(
        "trusted runs exist only on Linux and Windows",
    ))
}

/// Use the ordinary drive-letter form of a canonical Windows path, which
/// tools accept more widely than the `\\?\` form. Other prefixes, such as
/// network shares, are refused.
#[cfg(windows)]
fn native_form(path: PathBuf) -> Result<PathBuf, RunError> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::Prefix;
    match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(_) => Ok(path),
            Prefix::VerbatimDisk(_) => {
                let units: Vec<u16> = path.as_os_str().encode_wide().skip(4).collect();
                Ok(PathBuf::from(OsString::from_wide(&units)))
            }
            _ => Err(RunError::Refused(
                "run paths must be on a local drive letter",
            )),
        },
        _ => Err(RunError::Refused("run paths must be absolute")),
    }
}

#[cfg(not(windows))]
#[allow(clippy::unnecessary_wraps)]
fn native_form(path: PathBuf) -> Result<PathBuf, RunError> {
    Ok(path)
}

/// Open existing settings, waiting briefly for another command's lock.
fn open_control(data_root: &Path) -> Result<ControlStore, RunError> {
    let until = Instant::now() + CONTROL_WAIT;
    loop {
        match ControlStore::open_existing(data_root) {
            Ok(Some(store)) => return Ok(store),
            Ok(None) => {
                return Err(RunError::Refused("workspace control is not initialized"));
            }
            Err(ControlError::Busy) if Instant::now() < until => thread::sleep(CONTROL_POLL),
            Err(error) => return Err(error.into()),
        }
    }
}

/// Recent run records and pending runs.
pub fn audit(data_root: &Path, limit: usize) -> Result<RunAuditView, RunError> {
    let control = open_control(data_root)?;
    let (root, store_id) = control.file_audit_context()?;
    let journal = RunJournal::open(&root, store_id)?;
    Ok(RunAuditView {
        pending: journal.pending().to_vec(),
        entries: describe(journal.entries(limit)?)?,
    })
}

/// Records from an archived run journal of this store.
pub fn audit_archive(
    data_root: &Path,
    path: &Path,
    limit: usize,
) -> Result<RunAuditView, RunError> {
    let control = open_control(data_root)?;
    let (_, store_id) = control.file_audit_context()?;
    // A bare file name names a file in the current directory.
    let parent = match path.parent() {
        Some(parent) if parent.as_os_str().is_empty() => Path::new("."),
        Some(parent) => parent,
        None => return Err(RunError::Refused("the archive needs a parent directory")),
    };
    let directory = control.file_archive_directory(parent)?;
    let name = path
        .file_name()
        .ok_or(RunError::Refused("the archive needs a file name"))?;
    let journal = RunJournal::open_archive(&directory.join(name), store_id)?;
    Ok(RunAuditView {
        pending: journal.pending().to_vec(),
        entries: describe(journal.entries(limit)?)?,
    })
}

/// Clear a pending run after inspection. This retries nothing.
///
/// A run whose supervising process is still live is refused: its outcome is
/// still to come. A Linux run whose caller ended early can leave its scratch
/// directory; it is removed here. The result says whether one was found and
/// removed.
pub fn acknowledge(data_root: &Path, run_id: Uuid) -> Result<Option<bool>, RunError> {
    let control = open_control(data_root)?;
    let (root, store_id) = control.file_audit_context()?;
    // The released owner lock stays held here until the record is written.
    let Owner::Ended(owner) = probe_owner(&root, run_id)? else {
        return Err(RunError::Live(run_id));
    };
    RunJournal::open(&root, store_id)?.acknowledge(run_id)?;
    if owner.is_some() {
        drop(owner);
        let _ = fs::remove_file(owner_path(&root, run_id));
    }
    let scratch = root.join("run-scratch").join(run_id.to_string());
    Ok(match fs::symlink_metadata(&scratch) {
        Ok(metadata) if metadata.is_dir() => Some(fs::remove_dir_all(&scratch).is_ok()),
        _ => None,
    })
}

/// Move a run journal with no pending runs into an existing private
/// directory outside development data. The control lock is held throughout.
pub fn archive(data_root: &Path, archive_directory: &Path) -> Result<RunsArchived, RunError> {
    let control = open_control(data_root)?;
    let (root, store_id) = control.file_audit_context()?;
    let directory = control.file_archive_directory(archive_directory)?;
    let archived = RunJournal::archive(&root, store_id, &directory)?;
    Ok(RunsArchived {
        archive: NativePath::from_path(&archived.path)?,
        sha256: archived.sha256,
        event_count: archived.event_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_journal::arguments_digest;

    /// A registered workspace whose settings allow runs, and the journal
    /// root the runs use.
    fn store(directory: &Path) -> (PathBuf, WorkspaceId, PathBuf, Uuid) {
        let data = directory.join("data");
        let root = directory.join("dev");
        let workspace = root.join("work");
        fs::create_dir_all(&workspace).expect("workspace");
        let mut control = ControlStore::open(&data).expect("control store");
        control.set_development_root(&root, 0).expect("root");
        let id = control
            .register_workspace(&workspace, control.revision())
            .expect("register")
            .id();
        let permissions = PermissionSet {
            execute: true,
            ..PermissionSet::default()
        };
        control
            .set_permissions(id, permissions, control.revision())
            .expect("permissions");
        let (journal_root, store_id) = control.file_audit_context().expect("context");
        (data, id, journal_root, store_id)
    }

    /// A run of a profile that was never recorded, so the settings no longer
    /// allow it.
    fn withdrawn_start(workspace_id: WorkspaceId) -> RunStart {
        RunStart {
            run_id: Uuid::new_v4(),
            workspace_id,
            settings_revision: 3,
            profile_id: Uuid::new_v4(),
            profile_name: "removed".into(),
            profile_revision: 2,
            program: NativePath::from_path(&std::env::temp_dir().join("tool"))
                .expect("native path"),
            argument_count: 0,
            arguments_sha256: arguments_digest(&[]),
            working_directory: NativePath::from_path(&std::env::temp_dir()).expect("native path"),
            unix_sockets: SocketMode::PairsOnly,
            limits: None,
            network: None,
            package_cache: false,
        }
    }

    fn finished(result: RunResult) -> RunFinish {
        RunFinish {
            result,
            confinement: Some(platform_confinement()),
            elapsed_ms: 1,
            stdout_bytes: 0,
            stderr_bytes: 0,
            usage: None,
        }
    }

    /// A run of a recorded profile, so the settings still allow it on Linux.
    #[cfg(target_os = "linux")]
    fn allowed_start(data: &Path, directory: &Path, workspace_id: WorkspaceId) -> RunStart {
        let tools = directory.join("tools");
        fs::create_dir(&tools).expect("tools");
        let mut control = ControlStore::open(data).expect("control store");
        let profile = control
            .add_profile(
                &crate::control::ProfileDeclaration {
                    name: "kept".into(),
                    read_only_roots: vec![tools],
                    path_entries: Vec::new(),
                    environment: Vec::new(),
                },
                control.revision(),
            )
            .expect("profile");
        let mut start = withdrawn_start(workspace_id);
        start.profile_id = profile.id();
        start.profile_name = "kept".into();
        start
    }

    /// A withdrawn run whose cleanup was unconfirmed may still have
    /// processes, so a settings change never reports it stopped.
    /// Direct callers get the same range checks as the CLI flags, before
    /// anything is recorded.
    #[test]
    fn out_of_range_limits_are_refused_before_recording() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (data, workspace_id, journal_root, _store_id) = store(directory.path());
        let roomy = process::TreeLimits {
            max_memory_bytes: 1024 * 1024 * 1024,
            max_processes: 64,
            max_cpu_time: Duration::from_secs(60),
            max_disk_bytes: Some(1024 * 1024 * 1024),
        };
        let cases = [
            process::TreeLimits {
                max_memory_bytes: 1024 * 1024,
                ..roomy
            },
            process::TreeLimits {
                max_processes: 0,
                ..roomy
            },
            process::TreeLimits {
                max_cpu_time: Duration::ZERO,
                ..roomy
            },
            process::TreeLimits {
                max_disk_bytes: Some(1024),
                ..roomy
            },
            process::TreeLimits {
                max_disk_bytes: Some(MAX_TREE_DISK_BYTES + 1),
                ..roomy
            },
        ];
        for tree in cases {
            let request = RunRequest {
                workspace_id,
                profile: "system".into(),
                program: PathBuf::from("true"),
                arguments: Vec::new(),
                working_directory: None,
                host_services: false,
                network: false,
                limits: ProcessLimits {
                    tree: Some(tree),
                    ..ProcessLimits::default()
                },
            };
            let result = run(&data, &request, &CancelSignal::new());
            assert!(
                matches!(result, Err(RunError::InvalidLimits(_))),
                "{tree:?}: {result:?}"
            );
        }
        assert!(!journal_root.join("runs-v1.jsonl").exists());
    }

    #[test]
    fn withdrawn_runs_with_unconfirmed_cleanup_stay_unconfirmed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (data, workspace_id, journal_root, store_id) = store(directory.path());
        let start = withdrawn_start(workspace_id);
        let run_id = start.run_id;
        let mut journal = RunJournal::open(&journal_root, store_id).expect("journal");
        journal.start(start).expect("start");
        journal
            .finish(run_id, finished(RunResult::CleanupUnconfirmed))
            .expect("finish");
        drop(journal);
        let runs = stop_withdrawn_runs(&data, Duration::from_millis(100), &[]).expect("check");
        assert_eq!(runs.unconfirmed, vec![run_id]);
        assert!(runs.stopped.is_empty());
    }

    /// Acknowledging a run records no outcome, so it is not evidence that a
    /// withdrawn run stopped; only a recorded outcome is.
    #[test]
    fn acknowledged_runs_are_not_confirmed_as_stopped() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (_data, workspace_id, journal_root, store_id) = store(directory.path());
        let (abandoned, recorded) = (withdrawn_start(workspace_id), withdrawn_start(workspace_id));
        let (abandoned_id, recorded_id) = (abandoned.run_id, recorded.run_id);
        let mut journal = RunJournal::open(&journal_root, store_id).expect("journal");
        journal.start(abandoned).expect("start");
        journal.start(recorded).expect("start");
        journal.acknowledge(abandoned_id).expect("acknowledge");
        journal
            .finish(recorded_id, finished(RunResult::Revoked))
            .expect("finish");
        let runs = confirm_stopped(&journal_root, &journal, vec![abandoned_id, recorded_id])
            .expect("confirm");
        assert_eq!(runs.unconfirmed, vec![abandoned_id]);
        assert_eq!(runs.stopped, vec![recorded_id]);
    }

    /// Runs that a container revocation names are waited for even when the
    /// remaining settings would allow them. Linux only, for the reason given
    /// on `allowed_runs_are_not_waited_for`.
    #[cfg(target_os = "linux")]
    #[test]
    fn named_runs_are_waited_for_even_when_allowed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (data, workspace_id, journal_root, store_id) = store(directory.path());
        let start = allowed_start(&data, directory.path(), workspace_id);
        let run_id = start.run_id;
        let _owner = OwnerLock::acquire(&journal_root, run_id).expect("owner lock");
        RunJournal::open(&journal_root, store_id)
            .expect("journal")
            .start(start)
            .expect("start");
        let control = open_control(&data).expect("control");
        assert_eq!(
            workspace_runs(&control, workspace_id).expect("runs"),
            vec![run_id]
        );
        assert!(
            workspace_runs(&control, WorkspaceId::fresh())
                .expect("runs")
                .is_empty()
        );
        drop(control);
        let runs =
            stop_withdrawn_runs(&data, Duration::from_millis(100), &[run_id]).expect("check");
        assert_eq!(runs.unconfirmed, vec![run_id]);
    }

    /// The revocation count a run's watcher compares changes when a
    /// revocation of the workspace is requested. It survives the grant's
    /// removal and replay, and other workspaces keep their own count.
    #[test]
    fn revocation_counts_survive_the_removed_grant_and_replay() {
        use crate::control::ContainerGrantAccess;

        let directory = tempfile::tempdir().expect("temporary directory");
        let (data, workspace_id, _journal_root, _store_id) = store(directory.path());
        let tools = directory.path().join("tools");
        fs::create_dir(&tools).expect("tools");
        let mut control = ControlStore::open(&data).expect("control store");
        let grant_id =
            control.test_record_grant(workspace_id, &tools, ContainerGrantAccess::ReadExecute);
        assert_eq!(control.grant_revocations(workspace_id), 0);
        assert_eq!(
            control.container_grant_workspace(grant_id),
            Some(workspace_id)
        );
        control.test_record_revocation(grant_id);
        assert_eq!(control.grant_revocations(workspace_id), 1);
        assert_eq!(control.grant_revocations(WorkspaceId::fresh()), 0);
        drop(control);
        let reopened = open_control(&data).expect("control");
        assert!(reopened.container_grants().is_empty());
        assert_eq!(reopened.grant_revocations(workspace_id), 1);
    }

    /// A withdrawn run is reported as unconfirmed while its supervisor is
    /// live and while its outcome is missing, and drops out once recorded.
    #[test]
    fn withdrawn_runs_are_confirmed_only_when_their_outcome_is_recorded() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (data, workspace_id, journal_root, store_id) = store(directory.path());
        let start = withdrawn_start(workspace_id);
        let run_id = start.run_id;
        let owner = OwnerLock::acquire(&journal_root, run_id).expect("owner lock");
        RunJournal::open(&journal_root, store_id)
            .expect("journal")
            .start(start)
            .expect("start");

        // The supervisor still holds its lock, so the wait ends unconfirmed.
        let live = stop_withdrawn_runs(&data, Duration::from_millis(100), &[]).expect("check");
        assert_eq!(live.unconfirmed, vec![run_id]);
        assert!(live.stopped.is_empty());
        let error = acknowledge(&data, run_id).expect_err("a live run");
        assert!(matches!(error, RunError::Live(id) if id == run_id));

        // A supervisor that ended without an outcome is not a stopped run.
        drop(owner);
        let abandoned = stop_withdrawn_runs(&data, Duration::from_millis(100), &[]).expect("check");
        assert_eq!(abandoned.unconfirmed, vec![run_id]);

        let mut journal = RunJournal::open(&journal_root, store_id).expect("journal");
        journal
            .finish(
                run_id,
                RunFinish {
                    result: RunResult::Revoked,
                    confinement: Some(platform_confinement()),
                    elapsed_ms: 1,
                    stdout_bytes: 0,
                    stderr_bytes: 0,
                    usage: None,
                },
            )
            .expect("finish");
        drop(journal);
        let recorded = stop_withdrawn_runs(&data, Duration::from_millis(100), &[]).expect("check");
        assert!(recorded.unconfirmed.is_empty() && recorded.stopped.is_empty());
    }

    /// Network access needs the `task_network` preference on top of the
    /// execute, read, and write preferences, independently of host services.
    #[test]
    fn network_access_needs_the_task_network_preference() {
        let base = PermissionSet {
            execute: true,
            read: true,
            write: true,
            ..PermissionSet::default()
        };
        let networked = PermissionSet {
            task_network: true,
            ..base
        };
        assert_eq!(permitted(base, false, false), Ok(()));
        assert_eq!(
            permitted(base, false, true),
            Err("the workspace task_network preference is disabled")
        );
        assert_eq!(permitted(networked, false, true), Ok(()));
        assert_eq!(
            permitted(networked, true, true),
            Err("the workspace host_services preference is disabled")
        );
        let no_execute = PermissionSet {
            execute: false,
            ..networked
        };
        assert_eq!(
            permitted(no_execute, false, true),
            Err("the workspace execute preference is disabled")
        );
    }

    /// Turning `task_network` off withdraws a networked run but leaves a run
    /// without network access alone. Linux only, for the reason given on
    /// `allowed_runs_are_not_waited_for`.
    #[cfg(target_os = "linux")]
    #[test]
    fn disabling_task_network_withdraws_only_networked_runs() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (data, workspace_id, _journal_root, _store_id) = store(directory.path());
        let plain = allowed_start(&data, directory.path(), workspace_id);
        let mut networked = plain.clone();
        networked.network = Some(NetworkRecord {
            support_paths: Vec::new(),
        });
        let control = open_control(&data).expect("control");
        assert_eq!(withdrawn(&control, &plain), None);
        assert_eq!(
            withdrawn(&control, &networked),
            Some("the workspace task_network preference is disabled")
        );
        drop(control);
        let mut control = ControlStore::open(&data).expect("control store");
        let permissions = PermissionSet {
            execute: true,
            task_network: true,
            ..PermissionSet::default()
        };
        control
            .set_permissions(workspace_id, permissions, control.revision())
            .expect("permissions");
        drop(control);
        let control = open_control(&data).expect("control");
        assert_eq!(withdrawn(&control, &networked), None);
    }

    /// A live run that the settings still allow is not waited for. Linux
    /// only: on Windows the run would also need a real container grant on
    /// the workspace, which the CLI workflow tests cover.
    #[cfg(target_os = "linux")]
    #[test]
    fn allowed_runs_are_not_waited_for() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (data, workspace_id, journal_root, store_id) = store(directory.path());
        let start = allowed_start(&data, directory.path(), workspace_id);
        let _owner = OwnerLock::acquire(&journal_root, start.run_id).expect("owner lock");
        RunJournal::open(&journal_root, store_id)
            .expect("journal")
            .start(start)
            .expect("start");
        let none = stop_withdrawn_runs(&data, Duration::from_secs(5), &[]).expect("check");
        assert!(none.unconfirmed.is_empty() && none.stopped.is_empty());
    }
}
