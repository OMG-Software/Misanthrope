//! Bounded, single-writer records of trusted command runs.
//!
//! `runs-v1.jsonl` sits beside the control journal. Each run records its
//! intent before the program starts and its outcome after the tree is
//! observed empty. Records hold the profile, the program, a digest of the
//! arguments, and the result; never the argument text or the output. Several
//! runs may be in flight at once. The writer lock is held only while one
//! event is appended, so concurrent runs take turns. A run whose outcome was
//! never recorded, or whose cleanup was unconfirmed, stays pending until the
//! operator acknowledges it. A record never authorizes anything.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};
use uuid::{Uuid, Version};

use crate::policy::WorkspaceId;
use crate::session::NativePath;
use crate::storage::{self, StorageError};

const FILE_NAME: &str = "runs-v1.jsonl";
const SCHEMA_VERSION: u32 = 1;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 64 * 1024;
const MAX_EVENTS: usize = 4096;
const MAX_RECENT_ENTRIES: usize = 100;
/// Most resolver and certificate paths one networked run may record.
pub(crate) const MAX_NETWORK_SUPPORT_PATHS: usize = 32;
/// Most runs that may be pending at once, including unacknowledged ones.
pub(crate) const MAX_PENDING_RUNS: usize = 16;
/// Budget for one outcome or acknowledgement event. Real ones are about
/// 400 bytes; a test checks the largest form against this budget.
const OUTCOME_EVENT_RESERVE_BYTES: usize = 1024;
/// How long to wait for another run to finish its append.
const LOCK_WAIT: Duration = Duration::from_secs(10);
const LOCK_POLL: Duration = Duration::from_millis(20);

/// The Unix-domain socket mode a run used.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SocketMode {
    /// Socket pairs inside the tree only.
    PairsOnly,
    /// Also host Unix-domain sockets, by explicit operator choice.
    HostServices,
}

/// The confinement that applied to a run.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConfinementRecord {
    /// Landlock at this ABI, with the seccomp socket filter.
    Landlock { abi: u32 },
    /// A workspace AppContainer with no capabilities.
    AppContainer,
}

/// How a recorded run ended.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunResult {
    /// The program exited with this code.
    Exited { code: i64 },
    /// The program was ended by this Unix signal.
    Signalled { signal: i32 },
    /// The deadline elapsed and the tree was terminated.
    DeadlineExceeded,
    /// The operator cancelled the run and the tree was terminated.
    Cancelled,
    /// A settings change withdrew access the run relied on, and the tree was
    /// terminated.
    Revoked,
    /// Nothing started: a check, a confinement rule, or the start failed.
    NotStarted,
    /// Supervision failed after start; the tree was observed empty.
    SupervisionFailed,
    /// The tree or its output was not observed closed. Processes may remain.
    /// The run stays pending until it is acknowledged.
    CleanupUnconfirmed,
    /// The tree reached one of its limits and was terminated.
    LimitExceeded { limit: LimitKind },
}

/// A tree limit, as recorded.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitKind {
    /// Memory for the whole tree.
    Memory,
    /// Processes alive at once (tasks on Linux).
    Processes,
    /// CPU time summed over the tree.
    CpuTime,
    /// Net allocated-byte growth of the writable roots.
    Disk,
}

/// The tree limits a run started with.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsRecord {
    /// Memory for the whole tree, in bytes.
    pub max_memory_bytes: u64,
    /// Processes alive at once.
    pub max_processes: u32,
    /// CPU time for the whole tree, in milliseconds.
    pub max_cpu_ms: u64,
    /// Allocated-byte and file-length growth budget for the writable roots,
    /// in bytes. Runs recorded by 0.18.0 had no disk limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_disk_bytes: Option<u64>,
}

/// What a limited tree used.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UsageRecord {
    /// CPU time summed over the tree, in milliseconds.
    pub cpu_ms: u64,
    /// Peak memory, when the platform reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_memory_bytes: Option<u64>,
    /// Highest net disk growth observed above the pre-run baseline, when a
    /// disk limit applied. It is the larger of allocated-byte growth and
    /// file-length growth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_disk_bytes: Option<u64>,
}

/// The network access a run selected.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkRecord {
    /// Host resolver and certificate paths the run could read because it
    /// selected network access, as real paths. Linux only; empty on Windows.
    pub support_paths: Vec<NativePath>,
}

/// The intent recorded before a program starts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunStart {
    /// Durable run identifier.
    pub run_id: Uuid,
    /// The workspace the run used.
    #[serde(deserialize_with = "deserialize_workspace_id")]
    pub workspace_id: WorkspaceId,
    /// The settings revision the checks used.
    pub settings_revision: u64,
    /// The selected profile.
    pub profile_id: Uuid,
    /// The profile name at the time of the run.
    pub profile_name: String,
    /// The settings revision that recorded the profile.
    pub profile_revision: u64,
    /// The resolved program path.
    pub program: NativePath,
    /// How many arguments followed the program.
    pub argument_count: usize,
    /// SHA-256 of the arguments; see [`arguments_digest`].
    pub arguments_sha256: String,
    /// The working directory.
    pub working_directory: NativePath,
    /// The Unix-domain socket mode.
    pub unix_sockets: SocketMode,
    /// The tree limits. Every run since 0.17.0 records them; earlier runs
    /// had none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<LimitsRecord>,
    /// Present when the run selected network access, since 0.18.0. Earlier
    /// runs never had it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkRecord>,
    /// Whether the run could read the workspace's fetched-package cache,
    /// since 0.22.0. False and earlier runs omit it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub package_cache: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// The outcome recorded after a run.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunFinish {
    /// How the run ended.
    pub result: RunResult,
    /// The confinement in force, when the program started.
    pub confinement: Option<ConfinementRecord>,
    /// Milliseconds from start to exit or termination.
    pub elapsed_ms: u64,
    /// Every byte the tree wrote to standard output.
    pub stdout_bytes: u64,
    /// Every byte the tree wrote to standard error.
    pub stderr_bytes: u64,
    /// What a limited tree used, when its outcome was observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageRecord>,
}

/// One strict JSONL event.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunEntry {
    schema_version: u32,
    store_id: Uuid,
    event_id: Uuid,
    sequence: u64,
    timestamp: String,
    action: RunAction,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum RunAction {
    /// Bind the journal to one control store. Always first.
    Created,
    RunStarted {
        start: RunStart,
    },
    RunFinished {
        run_id: Uuid,
        finish: RunFinish,
    },
    RunAcknowledged {
        run_id: Uuid,
    },
}

fn deserialize_workspace_id<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<WorkspaceId, D::Error> {
    WorkspaceId::from_uuid(Uuid::deserialize(deserializer)?).map_err(serde::de::Error::custom)
}

/// A failure while opening, replaying, or changing the run journal.
#[derive(Debug, Error)]
pub enum RunJournalError {
    /// An operating-system persistence failure. The cause is retained.
    #[error("run journal I/O failed: {0}")]
    Io(#[from] io::Error),
    /// The existing journal is malformed or inconsistent. It is not repaired.
    #[error("invalid run journal: {0}")]
    Invalid(&'static str),
    /// Another run held the writer lock for longer than the wait.
    #[error("run journal is busy")]
    Busy,
    /// The journal has no room for another run and its outcome.
    #[error("run journal capacity reached; archive it")]
    LimitReached,
    /// Too many runs are pending; acknowledge finished ones first.
    #[error("too many runs are pending")]
    TooManyPending,
    /// No pending run has this identifier, or it cannot take this event.
    #[error("run {0} is not pending in a state that allows this")]
    WrongRun(Uuid),
    /// A run is pending, so the journal cannot be archived.
    #[error("run {0} is pending; acknowledge it before archiving")]
    Pending(Uuid),
    /// A write or sync failed; this handle cannot be used again.
    #[error("run journal writer is poisoned")]
    Poisoned,
    /// A move or directory sync had an uncertain outcome. Inspect both paths.
    #[error("run journal archive is uncertain; inspect {destination}: {source}", destination = destination.display())]
    ArchiveUncertain {
        destination: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl From<StorageError> for RunJournalError {
    fn from(value: StorageError) -> Self {
        match value {
            StorageError::Io(error) => Self::Io(error),
            StorageError::Invalid(reason) => Self::Invalid(reason),
            StorageError::Busy => Self::Busy,
        }
    }
}

/// Where a pending run is.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingRunState {
    /// The intent is recorded and no outcome yet. The run may still be
    /// going, or its caller may have ended without recording the outcome.
    InProgress,
    /// Cleanup was unconfirmed. Inspect, then acknowledge.
    NeedsAcknowledgement,
}

/// A pending run.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct PendingRun {
    /// The run identifier.
    pub run_id: Uuid,
    /// Its state.
    pub state: PendingRunState,
}

/// Receipt for an archived journal.
#[derive(Debug)]
pub(crate) struct ArchivedRuns {
    pub(crate) path: PathBuf,
    pub(crate) sha256: String,
    pub(crate) event_count: usize,
}

#[derive(Clone, Debug, Default)]
struct ReplayState {
    pending: Vec<PendingRun>,
    run_ids: HashSet<Uuid>,
}

impl ReplayState {
    /// Events that must stay free so every pending run can finish and be
    /// acknowledged.
    fn reserved_events(&self) -> usize {
        self.pending
            .iter()
            .map(|pending| match pending.state {
                PendingRunState::InProgress => 2,
                PendingRunState::NeedsAcknowledgement => 1,
            })
            .sum()
    }
}

/// A locked run journal. Drop it to release the lock.
pub(crate) struct RunJournal {
    file: File,
    store_id: Uuid,
    entries: Vec<RunEntry>,
    event_ids: HashSet<Uuid>,
    state: ReplayState,
    bytes: usize,
    poisoned: bool,
    read_only: bool,
}

impl RunJournal {
    /// Open or create the journal for `store_id`, waiting up to ten seconds
    /// for another run's append to finish.
    pub(crate) fn open(data_root: &Path, store_id: Uuid) -> Result<Self, RunJournalError> {
        Self::open_observed(data_root, store_id, || {})
    }

    /// Open the journal only if it exists, waiting like [`Self::open`].
    pub(crate) fn open_existing(
        data_root: &Path,
        store_id: Uuid,
    ) -> Result<Option<Self>, RunJournalError> {
        match fs::symlink_metadata(data_root.join(FILE_NAME)) {
            Ok(_) => Self::open(data_root, store_id).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// [`Self::open`], calling `on_busy` after each attempt that found the
    /// lock held. Tests use it to observe real contention.
    fn open_observed(
        data_root: &Path,
        store_id: Uuid,
        mut on_busy: impl FnMut(),
    ) -> Result<Self, RunJournalError> {
        if !valid_uuid(store_id) {
            return Err(RunJournalError::Invalid("invalid control store identifier"));
        }
        let until = Instant::now() + LOCK_WAIT;
        loop {
            match Self::try_open(data_root, store_id) {
                Err(RunJournalError::Busy) if Instant::now() < until => {
                    on_busy();
                    thread::sleep(LOCK_POLL);
                }
                result => return result,
            }
        }
    }

    fn try_open(data_root: &Path, store_id: Uuid) -> Result<Self, RunJournalError> {
        storage::create_private_directories(data_root)?;
        let path = data_root.join(FILE_NAME);
        match storage::open_locked_file(&path, true) {
            Ok(file) => {
                let mut journal = Self::empty(file, store_id);
                journal.append(RunAction::Created)?;
                storage::sync_parent_directory(data_root)?;
                Ok(journal)
            }
            Err(StorageError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists => {
                Self::replay(&path, store_id).map(|(journal, _)| journal)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Open an archived journal for inspection only.
    pub(crate) fn open_archive(path: &Path, store_id: Uuid) -> Result<Self, RunJournalError> {
        let (mut journal, _) = Self::replay(path, store_id)?;
        journal.read_only = true;
        Ok(journal)
    }

    /// Record a run's intent. It must be synced before the program starts.
    /// Room for its outcome and an acknowledgement is reserved.
    pub(crate) fn start(&mut self, start: RunStart) -> Result<(), RunJournalError> {
        if self.state.pending.len() >= MAX_PENDING_RUNS {
            return Err(RunJournalError::TooManyPending);
        }
        self.append(RunAction::RunStarted { start })
    }

    /// Record a run's outcome. `CleanupUnconfirmed` keeps it pending.
    pub(crate) fn finish(
        &mut self,
        run_id: Uuid,
        finish: RunFinish,
    ) -> Result<(), RunJournalError> {
        self.append(RunAction::RunFinished { run_id, finish })
    }

    /// Clear a pending run after the operator inspected it. This claims no
    /// outcome and retries nothing.
    pub(crate) fn acknowledge(&mut self, run_id: Uuid) -> Result<(), RunJournalError> {
        self.append(RunAction::RunAcknowledged { run_id })
    }

    /// Pending runs, oldest first.
    pub(crate) fn pending(&self) -> &[PendingRun] {
        &self.state.pending
    }

    /// The recorded intent of each pending run, oldest first: runs with no
    /// outcome yet and runs whose cleanup was unconfirmed.
    pub(crate) fn pending_starts(&self) -> Vec<&RunStart> {
        self.entries
            .iter()
            .filter_map(|entry| match &entry.action {
                RunAction::RunStarted { start } => Some(start),
                _ => None,
            })
            .filter(|start| {
                self.state
                    .pending
                    .iter()
                    .any(|pending| pending.run_id == start.run_id)
            })
            .collect()
    }

    /// The recorded result of a run, if its outcome was recorded. An
    /// acknowledged run with no outcome has none.
    pub(crate) fn outcome(&self, run_id: Uuid) -> Option<RunResult> {
        self.entries
            .iter()
            .rev()
            .find_map(|entry| match &entry.action {
                RunAction::RunFinished {
                    run_id: finished,
                    finish,
                } if *finished == run_id => Some(finish.result),
                _ => None,
            })
    }

    /// The newest `limit` entries, oldest first.
    pub(crate) fn entries(&self, limit: usize) -> Result<&[RunEntry], RunJournalError> {
        if !(1..=MAX_RECENT_ENTRIES).contains(&limit) {
            return Err(RunJournalError::Invalid(
                "entry limit must be between 1 and 100",
            ));
        }
        Ok(&self.entries[self.entries.len().saturating_sub(limit)..])
    }

    /// Move the journal, with no pending runs, into an operator-selected
    /// directory. The next run creates a new journal.
    pub(crate) fn archive(
        data_root: &Path,
        store_id: Uuid,
        archive_directory: &Path,
    ) -> Result<ArchivedRuns, RunJournalError> {
        Self::archive_with(
            data_root,
            store_id,
            archive_directory,
            omgcode_platform::journal::rename_noreplace,
            storage::sync_parent_directory,
        )
    }

    fn archive_with(
        data_root: &Path,
        store_id: Uuid,
        archive_directory: &Path,
        move_journal: impl FnOnce(&Path, &Path) -> io::Result<()>,
        sync_directory: impl Fn(&Path) -> Result<(), StorageError>,
    ) -> Result<ArchivedRuns, RunJournalError> {
        let source = data_root.join(FILE_NAME);
        let (journal, bytes) = Self::replay(&source, store_id)?;
        if let Some(pending) = journal.state.pending.first() {
            return Err(RunJournalError::Pending(pending.run_id));
        }
        let archived = ArchivedRuns {
            path: archive_directory.join(format!("runs-{}.jsonl", Uuid::new_v4())),
            sha256: hex(&Sha256::digest(&bytes)),
            event_count: journal.entries.len(),
        };
        // Windows journal handles deny delete sharing. The caller keeps the
        // control-store lock across the move.
        drop(journal);
        if let Err(source) = move_journal(&source, &archived.path) {
            return Err(match source.kind() {
                io::ErrorKind::AlreadyExists | io::ErrorKind::Unsupported => {
                    RunJournalError::Io(source)
                }
                _ => RunJournalError::ArchiveUncertain {
                    destination: archived.path,
                    source,
                },
            });
        }
        for directory in [archive_directory, data_root] {
            sync_directory(directory).map_err(|source| RunJournalError::ArchiveUncertain {
                destination: archived.path.clone(),
                source: match source {
                    StorageError::Io(error) => error,
                    error => io::Error::other(error),
                },
            })?;
        }
        Ok(archived)
    }

    fn replay(path: &Path, store_id: Uuid) -> Result<(Self, Vec<u8>), RunJournalError> {
        if !valid_uuid(store_id) {
            return Err(RunJournalError::Invalid("invalid control store identifier"));
        }
        match fs::symlink_metadata(path) {
            Ok(metadata) if !metadata.is_file() => {
                return Err(RunJournalError::Invalid(
                    "journal path is not a regular file",
                ));
            }
            Ok(_) => {}
            Err(error) => return Err(RunJournalError::Io(error)),
        }
        let mut file = storage::open_locked_file(path, false)?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(u64::try_from(MAX_BYTES + 1).map_err(|_| RunJournalError::LimitReached)?)
            .read_to_end(&mut bytes)?;
        if bytes.is_empty() || bytes.len() > MAX_BYTES || !bytes.ends_with(b"\n") {
            return Err(RunJournalError::Invalid(
                "empty, oversized, or incomplete run journal",
            ));
        }
        let mut journal = Self::empty(file, store_id);
        let complete = bytes
            .strip_suffix(b"\n")
            .ok_or(RunJournalError::Invalid("incomplete run journal"))?;
        for line in complete.split(|byte| *byte == b'\n') {
            if line.is_empty()
                || line
                    .len()
                    .checked_add(1)
                    .is_none_or(|length| length > MAX_EVENT_BYTES)
                || journal.entries.len() >= MAX_EVENTS
            {
                return Err(RunJournalError::Invalid("invalid event size or count"));
            }
            let value = storage::parse_strict_json(line)
                .map_err(|_| RunJournalError::Invalid("invalid or duplicate JSON keys"))?;
            let entry: RunEntry = serde_json::from_value(value)
                .map_err(|_| RunJournalError::Invalid("unsupported event shape"))?;
            if !journal.event_ids.insert(entry.event_id) {
                return Err(RunJournalError::Invalid("duplicate event identifier"));
            }
            Self::apply(&mut journal.state, &entry, journal.entries.len(), store_id)?;
            journal.entries.push(entry);
        }
        journal.bytes = bytes.len();
        Ok((journal, bytes))
    }

    fn empty(file: File, store_id: Uuid) -> Self {
        Self {
            file,
            store_id,
            entries: Vec::new(),
            event_ids: HashSet::new(),
            state: ReplayState::default(),
            bytes: 0,
            poisoned: false,
            read_only: false,
        }
    }

    /// Preflight the event and the reserve it needs, then write and sync it.
    fn append(&mut self, action: RunAction) -> Result<(), RunJournalError> {
        if self.read_only {
            return Err(RunJournalError::Invalid("an archived journal is read-only"));
        }
        if self.poisoned {
            return Err(RunJournalError::Poisoned);
        }
        if self.entries.len() >= MAX_EVENTS {
            return Err(RunJournalError::LimitReached);
        }
        let entry = RunEntry {
            schema_version: SCHEMA_VERSION,
            store_id: self.store_id,
            event_id: Uuid::new_v4(),
            sequence: u64::try_from(self.entries.len())
                .map_err(|_| RunJournalError::LimitReached)?,
            timestamp: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .map_err(|_| RunJournalError::Invalid("clock cannot be encoded"))?,
            action,
        };
        let mut next = self.state.clone();
        Self::apply(&mut next, &entry, self.entries.len(), self.store_id)?;
        let encoded = encode(&entry)?;
        let bytes = self
            .bytes
            .checked_add(encoded.len())
            .ok_or(RunJournalError::LimitReached)?;
        if encoded.len() > MAX_EVENT_BYTES || bytes > MAX_BYTES {
            return Err(RunJournalError::LimitReached);
        }
        // Outcomes and acknowledgements use the room that their start
        // reserved; every other event must leave that room free.
        if matches!(
            entry.action,
            RunAction::Created | RunAction::RunStarted { .. }
        ) {
            let reserved = next.reserved_events();
            let events = self
                .entries
                .len()
                .checked_add(1)
                .and_then(|count| count.checked_add(reserved));
            let total = reserved
                .checked_mul(OUTCOME_EVENT_RESERVE_BYTES)
                .and_then(|reserve| bytes.checked_add(reserve));
            if events.is_none_or(|count| count > MAX_EVENTS)
                || total.is_none_or(|size| size > MAX_BYTES)
            {
                return Err(RunJournalError::LimitReached);
            }
        }
        let written = self
            .file
            .seek(SeekFrom::End(0))
            .and_then(|_| self.file.write_all(&encoded))
            .and_then(|()| self.file.sync_all());
        if let Err(error) = written {
            self.poisoned = true;
            return Err(RunJournalError::Io(error));
        }
        self.event_ids.insert(entry.event_id);
        self.state = next;
        self.entries.push(entry);
        self.bytes = bytes;
        Ok(())
    }

    /// Validate one event against the preceding state.
    fn apply(
        state: &mut ReplayState,
        entry: &RunEntry,
        sequence: usize,
        store_id: Uuid,
    ) -> Result<(), RunJournalError> {
        let timestamp = OffsetDateTime::parse(&entry.timestamp, &Rfc3339)
            .map_err(|_| RunJournalError::Invalid("invalid event timestamp"))?;
        if entry.schema_version != SCHEMA_VERSION
            || entry.store_id != store_id
            || !valid_uuid(entry.event_id)
            || entry.sequence
                != u64::try_from(sequence).map_err(|_| RunJournalError::LimitReached)?
            || timestamp.offset() != UtcOffset::UTC
            || (sequence == 0) != matches!(entry.action, RunAction::Created)
        {
            return Err(RunJournalError::Invalid(
                "invalid event envelope or sequence",
            ));
        }
        match &entry.action {
            RunAction::Created => {}
            RunAction::RunStarted { start } => {
                if !valid_uuid(start.run_id)
                    || !valid_uuid(start.profile_id)
                    || state.run_ids.contains(&start.run_id)
                    || state.pending.len() >= MAX_PENDING_RUNS
                    || !valid_digest(&start.arguments_sha256)
                    || start.profile_name.is_empty()
                    || start.profile_name.len() > crate::control::MAX_PROFILE_NAME_CHARS
                    || start.limits.is_some_and(|limits| {
                        limits.max_memory_bytes == 0
                            || limits.max_processes == 0
                            || limits.max_cpu_ms == 0
                            || limits.max_disk_bytes == Some(0)
                    })
                    || start.network.as_ref().is_some_and(|network| {
                        network.support_paths.len() > MAX_NETWORK_SUPPORT_PATHS
                    })
                {
                    return Err(RunJournalError::Invalid("invalid or duplicate run start"));
                }
                state.run_ids.insert(start.run_id);
                state.pending.push(PendingRun {
                    run_id: start.run_id,
                    state: PendingRunState::InProgress,
                });
            }
            RunAction::RunFinished { run_id, finish } => {
                let index = state
                    .pending
                    .iter()
                    .position(|pending| {
                        pending.run_id == *run_id && pending.state == PendingRunState::InProgress
                    })
                    .ok_or(RunJournalError::WrongRun(*run_id))?;
                let started = finish.result != RunResult::NotStarted;
                if started != finish.confinement.is_some() {
                    return Err(RunJournalError::Invalid(
                        "an outcome must name its confinement exactly when the program started",
                    ));
                }
                if !started && finish.usage.is_some() {
                    return Err(RunJournalError::Invalid(
                        "a run that did not start has no usage",
                    ));
                }
                if finish.result == RunResult::CleanupUnconfirmed {
                    if let Some(pending) = state.pending.get_mut(index) {
                        pending.state = PendingRunState::NeedsAcknowledgement;
                    }
                } else {
                    state.pending.remove(index);
                }
            }
            RunAction::RunAcknowledged { run_id } => {
                let index = state
                    .pending
                    .iter()
                    .position(|pending| pending.run_id == *run_id)
                    .ok_or(RunJournalError::WrongRun(*run_id))?;
                state.pending.remove(index);
            }
        }
        Ok(())
    }
}

/// Describe recent entries as JSON for trusted inspection.
pub(crate) fn describe(entries: &[RunEntry]) -> Result<Vec<serde_json::Value>, RunJournalError> {
    entries
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<_, _>>()
        .map_err(|_| RunJournalError::Invalid("entry cannot be encoded"))
}

/// SHA-256 over the arguments in native form.
///
/// Each argument contributes its length in bytes as a little-endian `u64`,
/// then its bytes: the raw bytes on Unix, and the UTF-16 code units in
/// little-endian order on Windows. The text itself is never recorded.
pub fn arguments_digest(arguments: &[std::ffi::OsString]) -> String {
    let mut hasher = Sha256::new();
    for argument in arguments {
        let bytes = native_bytes(argument);
        hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(&bytes);
    }
    hex(&hasher.finalize())
}

#[cfg(unix)]
fn native_bytes(text: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    text.as_bytes().to_vec()
}

#[cfg(windows)]
fn native_bytes(text: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    text.encode_wide().flat_map(u16::to_le_bytes).collect()
}

#[cfg(not(any(unix, windows)))]
fn native_bytes(text: &std::ffi::OsStr) -> Vec<u8> {
    text.as_encoded_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn valid_digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn encode(entry: &RunEntry) -> Result<Vec<u8>, RunJournalError> {
    let mut encoded = serde_json::to_vec(entry)
        .map_err(|_| RunJournalError::Invalid("event cannot be encoded"))?;
    encoded.push(b'\n');
    Ok(encoded)
}

fn valid_uuid(value: Uuid) -> bool {
    !value.is_nil() && value.get_version() == Some(Version::Random)
}

#[cfg(test)]
mod tests;
