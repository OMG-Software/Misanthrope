//! Trusted operator command runs behind the workspace `execute` preference.
//!
//! [`run`] checks the live settings, resolves the program through the
//! selected toolchain profile, records the intent in the run journal, runs the
//! process tree under native confinement, and records the outcome. Linux uses
//! Landlock and the seccomp socket filter; Windows uses the workspace
//! AppContainer and its recorded grants. Nothing falls back to an unconfined
//! run. No model tool calls this. See `docs/execution.md` and ADR-0030.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use omgcode_platform::broker::RelativePath;
use omgcode_platform::process::{
    self, AppliedConfinement, CapturedOutput, Confinement, ProcessError, ProcessOutcome,
    ProcessSpec, Termination, UnixSocketAccess,
};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::control::{ControlError, ControlStore, ProfilePlatform, ToolchainProfile};
use crate::policy::WorkspaceId;
use crate::run_journal::{
    ConfinementRecord, PendingRun, RunFinish, RunJournal, RunJournalError, RunResult, RunStart,
    SocketMode, arguments_digest, describe,
};
use crate::session::{NativePath, SessionError};
use crate::workspace::DirectoryRecord;

pub use omgcode_platform::interrupt::cancel_on_interrupt;
pub use omgcode_platform::process::{CancelSignal, ProcessLimits};
pub use omgcode_platform::process::{MAX_CAPTURE_BYTES, MAX_DEADLINE};

/// How long to wait for another command's hold on the control journal.
const CONTROL_WAIT: Duration = Duration::from_secs(10);
const CONTROL_POLL: Duration = Duration::from_millis(20);
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
    /// Deadline, output, and cleanup bounds.
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

/// Check, record, run, and record the outcome of one command.
///
/// The control and run journal locks are held only while checking and while
/// appending, never while the program runs, so concurrent runs and settings
/// commands can proceed. Settings changed during a run apply to later runs.
/// `cancel` terminates the whole tree; the outcome is then `cancelled`.
pub fn run(
    data_root: &Path,
    request: &RunRequest,
    cancel: &CancelSignal,
) -> Result<RunReport, RunError> {
    let platform = ProfilePlatform::current().ok_or(RunError::Refused(
        "trusted runs exist only on Linux and Windows",
    ))?;
    let (journal_root, store_id, mut prepared) = {
        let control = open_control(data_root)?;
        let (journal_root, store_id) = control.file_audit_context()?;
        let prepared = prepare(&control, request, platform, &journal_root)?;
        let mut journal = RunJournal::open(&journal_root, store_id)?;
        journal.start(prepared.start.clone())?;
        (journal_root, store_id, prepared)
    };
    let run_id = prepared.start.run_id;
    let result = process::run_confined(
        &prepared.spec,
        &prepared.confinement,
        &request.limits,
        cancel,
    );
    let scratch_removed = prepared.scratch.as_mut().map(Scratch::remove);
    let (finish, outcome, failure) = settle(run_id, result);
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
    };
    let recorded = RunJournal::open(&journal_root, store_id)
        .and_then(|mut journal| journal.finish(run_id, finish));
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

/// Map the runner's result to a recorded outcome and, for failures, an error.
fn settle(
    run_id: Uuid,
    result: Result<ProcessOutcome, ProcessError>,
) -> (RunFinish, Option<ProcessOutcome>, Option<RunError>) {
    match result {
        Ok(outcome) => {
            let finish = RunFinish {
                result: match outcome.termination {
                    Termination::Exited { code } => RunResult::Exited { code },
                    Termination::Signalled { signal } => RunResult::Signalled { signal },
                    Termination::DeadlineExceeded => RunResult::DeadlineExceeded,
                    Termination::Cancelled => RunResult::Cancelled,
                },
                confinement: Some(record_confinement(outcome.confinement)),
                elapsed_ms: millis(outcome.elapsed),
                stdout_bytes: outcome.stdout.total_bytes,
                stderr_bytes: outcome.stderr.total_bytes,
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
        AppliedConfinement::AppContainer | AppliedConfinement::Unconfined => {
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
    let registration = control
        .workspace(request.workspace_id)
        .ok_or(ControlError::UnknownWorkspace)?;
    let permissions = registration.permissions();
    let scope = control.file_scope(request.workspace_id)?;
    if !permissions.execute {
        return Err(RunError::Refused(
            "the workspace execute preference is disabled",
        ));
    }
    if request.host_services {
        if platform == ProfilePlatform::Windows {
            return Err(RunError::Refused(
                "host-service access is not available on Windows",
            ));
        }
        if !permissions.host_services {
            return Err(RunError::Refused(
                "the workspace host_services preference is disabled",
            ));
        }
    }
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
    let (confinement, scratch) = platform_setup(
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

/// Build the platform confinement and the runner-owned variables.
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn platform_setup(
    _control: &ControlStore,
    request: &RunRequest,
    _workspace_record: &DirectoryRecord,
    workspace: &Path,
    roots: Vec<PathBuf>,
    data_root: &Path,
    run_id: Uuid,
    environment: &mut Vec<(OsString, OsString)>,
) -> Result<(Confinement, Option<Scratch>), RunError> {
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
    let confinement = Confinement {
        read_only,
        read_write: vec![workspace.to_path_buf(), path, PathBuf::from("/dev/null")],
        unix_sockets: if request.host_services {
            UnixSocketAccess::HostServices
        } else {
            UnixSocketAccess::PairsOnly
        },
        container: None,
    };
    Ok((confinement, Some(scratch)))
}

/// Build the platform confinement and the runner-owned variables.
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
    _data_root: &Path,
    _run_id: Uuid,
    environment: &mut Vec<(OsString, OsString)>,
) -> Result<(Confinement, Option<Scratch>), RunError> {
    use crate::control::{ContainerGrantAccess, ContainerGrantState, container_name};

    let grants: Vec<_> = control
        .container_grants()
        .iter()
        .filter(|status| status.grant().workspace_id() == request.workspace_id)
        .collect();
    if grants
        .iter()
        .any(|status| status.state() != ContainerGrantState::Active)
    {
        return Err(RunError::Refused(
            "a container grant of this workspace is pending or being revoked; revoke it first",
        ));
    }
    if !grants.iter().any(|status| {
        status.grant().access() == ContainerGrantAccess::ReadWrite
            && status.grant().directory().same_directory(workspace_record)
    }) {
        return Err(RunError::Refused(
            "grant the workspace directory read-write to its container first",
        ));
    }
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
    let confinement = Confinement {
        read_only: roots,
        read_write: vec![workspace.to_path_buf()],
        unix_sockets: UnixSocketAccess::PairsOnly,
        container: Some(name),
    };
    Ok((confinement, None))
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
) -> Result<(Confinement, Option<Scratch>), RunError> {
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
    let parent = path
        .parent()
        .ok_or(RunError::Refused("the archive needs a parent directory"))?;
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
/// A Linux run whose caller ended early can leave its scratch directory;
/// it is removed here. The result says whether one was found and removed.
pub fn acknowledge(data_root: &Path, run_id: Uuid) -> Result<Option<bool>, RunError> {
    let control = open_control(data_root)?;
    let (root, store_id) = control.file_audit_context()?;
    RunJournal::open(&root, store_id)?.acknowledge(run_id)?;
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
