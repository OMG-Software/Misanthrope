//! Bounded, single-writer audit records for native file operations.
//!
//! The log records intent and outcome metadata only. It never stores file data,
//! executes work, or treats an audit record as authorization.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use misanthrope_platform::broker::RelativePath;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};
use uuid::{Uuid, Version};

use crate::policy::WorkspaceId;
use crate::session::NativePath;
use crate::storage::{self, StorageError};

const FILE_NAME: &str = "file-operations-v1.jsonl";
const SCHEMA_VERSION: u32 = 1;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 64 * 1024;
const MAX_EVENTS: usize = 4096;
const MAX_RECENT_ENTRIES: usize = 100;
const TERMINAL_EVENT_RESERVE_BYTES: usize = 2048;
const COMPLETION_AND_ACK_RESERVE_BYTES: usize = 2 * TERMINAL_EVENT_RESERVE_BYTES;

/// The file operation recorded by the trusted broker.
///
/// This is descriptive audit metadata. It does not grant access or report that
/// an operation was authorized.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FileOperation {
    /// Read an existing file.
    Read,
    /// List a directory, including the workspace root.
    List,
    /// Create a new file without replacing an existing entry.
    CreateFile,
    /// Create one directory.
    CreateDirectory,
}

/// The observed result of one file operation.
///
/// `Uncertain` means a side effect may have happened. The audit never retries it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FileOutcome {
    /// The operation completed successfully.
    Succeeded,
    /// The operation failed without an ambiguous result.
    Failed,
    /// The operation may have changed the filesystem.
    Uncertain,
}

/// One strict JSONL event. It contains operation metadata, never file contents.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuditEntry {
    pub(crate) schema_version: u32,
    pub(crate) store_id: Uuid,
    pub(crate) event_id: Uuid,
    pub(crate) sequence: u64,
    pub(crate) timestamp: String,
    pub(crate) action: FileAuditAction,
}

/// A durable event in the file-operation audit log.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(crate) enum FileAuditAction {
    /// Bind this log to one control store. This must be the first event.
    Created,
    /// Persist intent before the broker performs any filesystem operation.
    OperationStarted {
        operation_id: Uuid,
        session_id: Uuid,
        #[serde(deserialize_with = "deserialize_workspace_id")]
        workspace_id: WorkspaceId,
        revision: u64,
        operation: FileOperation,
        target: NativePath,
    },
    /// Record the result for the one active intent.
    OperationFinished {
        operation_id: Uuid,
        outcome: FileOutcome,
    },
    /// Clear unresolved recovery state without claiming success or retrying I/O.
    OperationAcknowledged { operation_id: Uuid },
}

/// Reject malformed persisted IDs before constructing typed workspace metadata.
fn deserialize_workspace_id<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<WorkspaceId, D::Error> {
    WorkspaceId::from_uuid(Uuid::deserialize(deserializer)?).map_err(serde::de::Error::custom)
}

/// A failure while opening, replaying, or changing the file-operation log.
#[derive(Debug, Error)]
pub(crate) enum FileAuditError {
    /// An operating-system persistence failure. The original cause is retained.
    #[error("file-operation audit I/O failed: {0}")]
    Io(#[from] io::Error),
    /// The existing audit log is malformed or inconsistent. It is not repaired.
    #[error("invalid file-operation audit log: {0}")]
    Invalid(&'static str),
    /// Another caller owns the exclusive writer lock.
    #[error("file-operation audit log is already open")]
    Busy,
    /// An unresolved operation blocks a new intent.
    #[error("file operation {0} is unresolved")]
    PendingOperation(Uuid),
    /// An operation ID does not identify the active, finishable operation.
    #[error("file operation does not identify the active operation")]
    WrongOperation,
    /// The log reached its bounded event or byte capacity.
    #[error("file-operation audit capacity reached")]
    LimitReached,
    /// A write or sync failed; this handle cannot be mutated again.
    #[error("file-operation audit writer is poisoned")]
    Poisoned,
}

impl From<StorageError> for FileAuditError {
    /// Preserve I/O causes and lock conflicts across the storage boundary.
    fn from(value: StorageError) -> Self {
        match value {
            StorageError::Io(error) => Self::Io(error),
            StorageError::Invalid(reason) => Self::Invalid(reason),
            StorageError::Busy => Self::Busy,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingState {
    InProgress,
    NeedsAcknowledgement,
    NeedsRecovery,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingOperation {
    id: Uuid,
    state: PendingState,
}

#[derive(Clone, Debug, Default)]
struct ReplayState {
    pending: Option<PendingOperation>,
    operation_ids: HashSet<Uuid>,
    last_revision: Option<u64>,
}

/// A locked, append-only audit log for native file-operation intent and outcome.
///
/// The lock lasts until this value is dropped. Start records are synced before
/// the broker may perform I/O. Reopening an unfinished or uncertain operation
/// requires an explicit acknowledgement and never retries the filesystem call.
pub(crate) struct FileAudit {
    file: File,
    store_id: Uuid,
    entries: Vec<AuditEntry>,
    event_ids: HashSet<Uuid>,
    state: ReplayState,
    bytes: usize,
    poisoned: bool,
    #[cfg(test)]
    fail_next_write: bool,
    #[cfg(test)]
    fail_next_sync: bool,
}

impl FileAudit {
    /// Open or initialize the audit log linked to `store_id` under an exclusive lock.
    ///
    /// New data directories use the shared private-directory helper. Existing
    /// empty, corrupt, unsupported, or incomplete files fail without reset.
    pub(crate) fn open(data_root: &Path, store_id: Uuid) -> Result<Self, FileAuditError> {
        if !valid_uuid(store_id) {
            return Err(FileAuditError::Invalid("invalid control store identifier"));
        }
        storage::create_private_directories(data_root)?;
        let path = data_root.join(FILE_NAME);
        match storage::open_locked_file(&path, true) {
            Ok(file) => {
                let mut audit = Self::empty(file, store_id);
                audit.append(FileAuditAction::Created)?;
                storage::sync_parent_directory(data_root)?;
                Ok(audit)
            }
            Err(StorageError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists => {
                Self::open_existing(data_root, store_id)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Return the unresolved operation, if any.
    ///
    /// A returned ID needs explicit acknowledgement before another operation
    /// can start. This method does not imply that the operation succeeded.
    pub(crate) fn pending(&self) -> Option<Uuid> {
        self.state.pending.map(|pending| pending.id)
    }

    /// Return the newest `limit` entries in chronological order.
    pub(crate) fn entries(&self, limit: usize) -> Result<Vec<AuditEntry>, FileAuditError> {
        if !(1..=MAX_RECENT_ENTRIES).contains(&limit) {
            return Err(FileAuditError::Invalid(
                "entry limit must be between 1 and 100",
            ));
        }
        let start = self.entries.len().saturating_sub(limit);
        Ok(self.entries[start..].to_vec())
    }

    /// Durably record an operation intent before any corresponding file I/O.
    ///
    /// The target is validated as a native workspace-relative path. The empty
    /// path is valid only for a directory listing. Capacity reserves both a
    /// finish event and a later acknowledgement before this intent is written.
    pub(crate) fn start(
        &mut self,
        session_id: Uuid,
        workspace_id: WorkspaceId,
        revision: u64,
        operation: FileOperation,
        target: NativePath,
    ) -> Result<Uuid, FileAuditError> {
        self.ensure_healthy()?;
        if let Some(pending) = self.state.pending {
            return Err(FileAuditError::PendingOperation(pending.id));
        }
        if !valid_uuid(session_id) || !valid_uuid(*workspace_id.as_uuid()) {
            return Err(FileAuditError::Invalid(
                "invalid operation scope identifier",
            ));
        }
        validate_target(operation, &target)?;
        let operation_id = Uuid::new_v4();
        let action = FileAuditAction::OperationStarted {
            operation_id,
            session_id,
            workspace_id,
            revision,
            operation,
            target,
        };
        self.append_with_reserve(action, 2, COMPLETION_AND_ACK_RESERVE_BYTES)?;
        Ok(operation_id)
    }

    /// Record a terminal outcome for the live operation.
    ///
    /// `Uncertain` retains a pending marker. The operation cannot be finished
    /// again; a trusted caller must acknowledge the ambiguity explicitly.
    pub(crate) fn finish(
        &mut self,
        operation_id: Uuid,
        outcome: FileOutcome,
    ) -> Result<(), FileAuditError> {
        self.ensure_healthy()?;
        let Some(pending) = self.state.pending else {
            return Err(FileAuditError::WrongOperation);
        };
        if pending.id != operation_id || pending.state != PendingState::InProgress {
            return Err(FileAuditError::WrongOperation);
        }
        self.append(FileAuditAction::OperationFinished {
            operation_id,
            outcome,
        })
    }

    /// Acknowledge unresolved state without claiming success or replaying I/O.
    pub(crate) fn acknowledge(&mut self, operation_id: Uuid) -> Result<(), FileAuditError> {
        self.ensure_healthy()?;
        if self.state.pending.map(|pending| pending.id) != Some(operation_id) {
            return Err(FileAuditError::WrongOperation);
        }
        self.append(FileAuditAction::OperationAcknowledged { operation_id })
    }

    /// Open and replay an existing log without creating or replacing anything.
    fn open_existing(data_root: &Path, expected_store_id: Uuid) -> Result<Self, FileAuditError> {
        let path = data_root.join(FILE_NAME);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.is_file() => {
                return Err(FileAuditError::Invalid("audit path is not a regular file"));
            }
            Ok(_) => {}
            Err(error) => return Err(FileAuditError::Io(error)),
        }
        let mut file = storage::open_locked_file(&path, false)?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(u64::try_from(MAX_BYTES + 1).map_err(|_| FileAuditError::LimitReached)?)
            .read_to_end(&mut bytes)?;
        if bytes.is_empty() || bytes.len() > MAX_BYTES || !bytes.ends_with(b"\n") {
            return Err(FileAuditError::Invalid(
                "empty, oversized, or incomplete audit log",
            ));
        }
        let mut audit = Self::empty(file, expected_store_id);
        let complete = bytes
            .strip_suffix(b"\n")
            .ok_or(FileAuditError::Invalid("incomplete audit log"))?;
        for line in complete.split(|byte| *byte == b'\n') {
            let line_ending_bytes = if line.ends_with(b"\r") { 2 } else { 1 };
            if line.is_empty()
                || line
                    .len()
                    .checked_add(line_ending_bytes)
                    .is_none_or(|length| length > MAX_EVENT_BYTES)
                || audit.entries.len() >= MAX_EVENTS
            {
                return Err(FileAuditError::Invalid("invalid event size or count"));
            }
            let value = storage::parse_strict_json(line)
                .map_err(|_| FileAuditError::Invalid("invalid or duplicate JSON keys"))?;
            let entry: AuditEntry = serde_json::from_value(value)
                .map_err(|_| FileAuditError::Invalid("unsupported event shape"))?;
            audit.replay_entry(entry)?;
        }
        if let Some(pending) = &mut audit.state.pending
            && pending.state == PendingState::InProgress
        {
            pending.state = PendingState::NeedsRecovery;
        }
        audit.bytes = bytes.len();
        Ok(audit)
    }

    /// Create an empty in-memory state around a newly locked file.
    fn empty(file: File, store_id: Uuid) -> Self {
        Self {
            file,
            store_id,
            entries: Vec::new(),
            event_ids: HashSet::new(),
            state: ReplayState::default(),
            bytes: 0,
            poisoned: false,
            #[cfg(test)]
            fail_next_write: false,
            #[cfg(test)]
            fail_next_sync: false,
        }
    }

    /// Apply an event to a candidate replay state, then write and sync it.
    fn append(&mut self, action: FileAuditAction) -> Result<(), FileAuditError> {
        self.append_with_reserve(action, 0, 0)
    }

    /// Preflight both the event and any capacity that the active intent needs.
    fn append_with_reserve(
        &mut self,
        action: FileAuditAction,
        reserved_events: usize,
        reserved_bytes: usize,
    ) -> Result<(), FileAuditError> {
        self.ensure_healthy()?;
        let entry = self.make_entry(action)?;
        if self.event_ids.contains(&entry.event_id) {
            return Err(FileAuditError::Invalid("duplicate event identifier"));
        }
        let mut next_state = self.state.clone();
        Self::apply(&mut next_state, &entry, self.entries.len(), self.store_id)?;
        let encoded = encode_entry(&entry)?;
        if encoded.len() > MAX_EVENT_BYTES {
            return Err(FileAuditError::LimitReached);
        }
        let total_bytes = self
            .bytes
            .checked_add(encoded.len())
            .ok_or(FileAuditError::LimitReached)?;
        let candidate_bytes = total_bytes
            .checked_add(reserved_bytes)
            .ok_or(FileAuditError::LimitReached)?;
        let candidate_events = self
            .entries
            .len()
            .checked_add(1)
            .and_then(|count| count.checked_add(reserved_events))
            .ok_or(FileAuditError::LimitReached)?;
        if candidate_bytes > MAX_BYTES || candidate_events > MAX_EVENTS {
            return Err(FileAuditError::LimitReached);
        }
        if reserved_bytes > 0 {
            self.validate_terminal_reserve(&entry)?;
        }
        if let Err(error) = self.write_and_sync(&encoded) {
            self.poisoned = true;
            return Err(FileAuditError::Io(error));
        }
        self.event_ids.insert(entry.event_id);
        self.state = next_state;
        self.entries.push(entry);
        self.bytes = total_bytes;
        Ok(())
    }

    /// Ensure reserved terminal records fit both their byte and event bounds.
    fn validate_terminal_reserve(&self, start: &AuditEntry) -> Result<(), FileAuditError> {
        let FileAuditAction::OperationStarted { operation_id, .. } = start.action else {
            return Err(FileAuditError::Invalid(
                "completion reserve requires an operation start",
            ));
        };
        let sequence = u64::try_from(self.entries.len())
            .map_err(|_| FileAuditError::LimitReached)?
            .checked_add(1)
            .ok_or(FileAuditError::LimitReached)?;
        let mut total = 0usize;
        for action in [
            FileAuditAction::OperationFinished {
                operation_id,
                outcome: FileOutcome::Uncertain,
            },
            FileAuditAction::OperationAcknowledged { operation_id },
        ] {
            let entry = AuditEntry {
                schema_version: SCHEMA_VERSION,
                store_id: self.store_id,
                event_id: Uuid::new_v4(),
                sequence,
                // Use the longest valid RFC 3339 precision for the reserve
                // calculation, independent of the start event's clock value.
                timestamp: "9999-12-31T23:59:59.999999999Z".to_owned(),
                action,
            };
            let encoded = encode_entry(&entry)?;
            if encoded.len() > TERMINAL_EVENT_RESERVE_BYTES {
                return Err(FileAuditError::Invalid(
                    "terminal event exceeds reserved capacity",
                ));
            }
            total = total
                .checked_add(encoded.len())
                .ok_or(FileAuditError::LimitReached)?;
        }
        if total > COMPLETION_AND_ACK_RESERVE_BYTES {
            return Err(FileAuditError::Invalid(
                "terminal events exceed reserved capacity",
            ));
        }
        Ok(())
    }

    /// Generate a bounded event envelope using one UTC timestamp.
    fn make_entry(&self, action: FileAuditAction) -> Result<AuditEntry, FileAuditError> {
        if self.entries.len() >= MAX_EVENTS {
            return Err(FileAuditError::LimitReached);
        }
        Ok(AuditEntry {
            schema_version: SCHEMA_VERSION,
            store_id: self.store_id,
            event_id: Uuid::new_v4(),
            sequence: u64::try_from(self.entries.len())
                .map_err(|_| FileAuditError::LimitReached)?,
            timestamp: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .map_err(|_| FileAuditError::Invalid("clock cannot be encoded"))?,
            action,
        })
    }

    /// Validate one event against the complete preceding state.
    fn apply(
        state: &mut ReplayState,
        entry: &AuditEntry,
        expected_sequence: usize,
        expected_store_id: Uuid,
    ) -> Result<(), FileAuditError> {
        let timestamp = OffsetDateTime::parse(&entry.timestamp, &Rfc3339)
            .map_err(|_| FileAuditError::Invalid("invalid event timestamp"))?;
        if entry.schema_version != SCHEMA_VERSION
            || entry.store_id != expected_store_id
            || !valid_uuid(entry.event_id)
            || entry.sequence
                != u64::try_from(expected_sequence).map_err(|_| FileAuditError::LimitReached)?
            || timestamp.offset() != UtcOffset::UTC
        {
            return Err(FileAuditError::Invalid(
                "invalid event envelope or sequence",
            ));
        }
        match &entry.action {
            FileAuditAction::Created => {
                if expected_sequence != 0
                    || state.pending.is_some()
                    || !state.operation_ids.is_empty()
                {
                    return Err(FileAuditError::Invalid("created event is not first"));
                }
            }
            FileAuditAction::OperationStarted {
                operation_id,
                session_id,
                workspace_id,
                revision,
                operation,
                target,
            } => {
                if expected_sequence == 0
                    || state.pending.is_some()
                    || !valid_uuid(*operation_id)
                    || !valid_uuid(*session_id)
                    || !valid_uuid(*workspace_id.as_uuid())
                    || state.operation_ids.contains(operation_id)
                    || state.last_revision.is_some_and(|last| *revision < last)
                {
                    return Err(FileAuditError::Invalid(
                        "invalid or duplicate operation scope",
                    ));
                }
                validate_target(*operation, target)?;
                state.operation_ids.insert(*operation_id);
                state.last_revision = Some(*revision);
                state.pending = Some(PendingOperation {
                    id: *operation_id,
                    state: PendingState::InProgress,
                });
            }
            FileAuditAction::OperationFinished {
                operation_id,
                outcome,
            } => {
                let Some(pending) = state.pending else {
                    return Err(FileAuditError::Invalid("finish has no matching operation"));
                };
                if pending.id != *operation_id || pending.state != PendingState::InProgress {
                    return Err(FileAuditError::Invalid(
                        "finish does not match a live operation",
                    ));
                }
                match outcome {
                    FileOutcome::Succeeded | FileOutcome::Failed => state.pending = None,
                    FileOutcome::Uncertain => {
                        state.pending = Some(PendingOperation {
                            id: *operation_id,
                            state: PendingState::NeedsAcknowledgement,
                        });
                    }
                }
            }
            FileAuditAction::OperationAcknowledged { operation_id } => {
                if state.pending.map(|pending| pending.id) != Some(*operation_id) {
                    return Err(FileAuditError::Invalid(
                        "acknowledgement has no matching operation",
                    ));
                }
                state.pending = None;
            }
        }
        Ok(())
    }

    /// Replay one parsed event and retain its validated identifier.
    fn replay_entry(&mut self, entry: AuditEntry) -> Result<(), FileAuditError> {
        if self.event_ids.contains(&entry.event_id) {
            return Err(FileAuditError::Invalid("duplicate event identifier"));
        }
        let mut next_state = self.state.clone();
        Self::apply(&mut next_state, &entry, self.entries.len(), self.store_id)?;
        self.event_ids.insert(entry.event_id);
        self.state = next_state;
        self.entries.push(entry);
        Ok(())
    }

    /// Append only after candidate state and reserved capacity pass validation.
    fn write_and_sync(&mut self, encoded: &[u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::End(0))?;
        #[cfg(test)]
        if self.fail_next_write {
            self.fail_next_write = false;
            return Err(io::Error::other("injected audit write failure"));
        }
        self.file.write_all(encoded)?;
        #[cfg(test)]
        if self.fail_next_sync {
            self.fail_next_sync = false;
            return Err(io::Error::other("injected audit sync failure"));
        }
        self.file.sync_all()
    }

    /// Reject mutations after an uncertain write or sync result.
    fn ensure_healthy(&self) -> Result<(), FileAuditError> {
        if self.poisoned {
            Err(FileAuditError::Poisoned)
        } else {
            Ok(())
        }
    }

    /// Inject one instance-local sync failure for broker recovery tests.
    #[cfg(test)]
    pub(crate) fn fail_next_sync_for_test(&mut self) {
        self.fail_next_sync = true;
    }
}

/// Validate a native relative target before persistence or replay acceptance.
fn validate_target(operation: FileOperation, target: &NativePath) -> Result<(), FileAuditError> {
    let path = target
        .to_path_buf()
        .map_err(|_| FileAuditError::Invalid("target path encoding does not match this OS"))?;
    let relative = RelativePath::new(&path)
        .map_err(|_| FileAuditError::Invalid("target is not a valid workspace-relative path"))?;
    if relative.is_empty() && operation != FileOperation::List {
        return Err(FileAuditError::Invalid(
            "workspace root is valid only for listing",
        ));
    }
    Ok(())
}

/// Encode exactly one JSON event and its terminating newline.
fn encode_entry(entry: &AuditEntry) -> Result<Vec<u8>, FileAuditError> {
    let mut encoded = serde_json::to_vec(entry)
        .map_err(|_| FileAuditError::Invalid("event cannot be encoded"))?;
    encoded.push(b'\n');
    Ok(encoded)
}

/// Accept only non-nil version-4 identifiers for log and operation scope.
fn valid_uuid(value: Uuid) -> bool {
    !value.is_nil() && value.get_version() == Some(Version::Random)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn scope() -> (Uuid, WorkspaceId) {
        (
            Uuid::new_v4(),
            WorkspaceId::from_uuid(Uuid::new_v4()).expect("fresh workspace ID"),
        )
    }

    fn native_target(path: &Path) -> NativePath {
        NativePath::from_path(path).expect("native test path")
    }

    fn valid_target() -> NativePath {
        native_target(Path::new("notes.txt"))
    }

    #[test]
    fn records_successful_pairs_and_reopens_the_bound_log() {
        let directory = tempfile::tempdir().expect("temporary data directory");
        let store_id = Uuid::new_v4();
        let (session_id, workspace_id) = scope();
        let mut audit = FileAudit::open(directory.path(), store_id).expect("open audit");
        let operation_id = audit
            .start(
                session_id,
                workspace_id,
                7,
                FileOperation::Read,
                valid_target(),
            )
            .expect("record intent");
        audit
            .finish(operation_id, FileOutcome::Succeeded)
            .expect("record success");
        assert_eq!(audit.pending(), None);
        assert_eq!(audit.entries(10).expect("entries").len(), 3);
        drop(audit);

        let reopened = FileAudit::open(directory.path(), store_id).expect("reopen audit");
        assert_eq!(reopened.pending(), None);
        assert_eq!(reopened.entries(10).expect("replayed entries").len(), 3);
        drop(reopened);
        assert!(matches!(
            FileAudit::open(directory.path(), Uuid::new_v4()),
            Err(FileAuditError::Invalid(_))
        ));
    }

    #[test]
    fn unfinished_and_uncertain_operations_require_acknowledgement() {
        let directory = tempfile::tempdir().expect("temporary data directory");
        let store_id = Uuid::new_v4();
        let (session_id, workspace_id) = scope();
        let mut audit = FileAudit::open(directory.path(), store_id).expect("open audit");
        let operation_id = audit
            .start(
                session_id,
                workspace_id,
                1,
                FileOperation::CreateFile,
                valid_target(),
            )
            .expect("record intent");
        drop(audit);

        let mut reopened =
            FileAudit::open(directory.path(), store_id).expect("reopen pending audit");
        assert_eq!(reopened.pending(), Some(operation_id));
        assert!(matches!(
            reopened.start(session_id, workspace_id, 1, FileOperation::Read, valid_target()),
            Err(FileAuditError::PendingOperation(id)) if id == operation_id
        ));
        assert!(matches!(
            reopened.finish(operation_id, FileOutcome::Succeeded),
            Err(FileAuditError::WrongOperation)
        ));
        reopened
            .acknowledge(operation_id)
            .expect("acknowledge recovery");
        assert_eq!(reopened.pending(), None);
        drop(reopened);

        let mut reopened =
            FileAudit::open(directory.path(), store_id).expect("reopen cleared audit");
        let uncertain = reopened
            .start(
                session_id,
                workspace_id,
                2,
                FileOperation::CreateDirectory,
                valid_target(),
            )
            .expect("start second operation");
        reopened
            .finish(uncertain, FileOutcome::Uncertain)
            .expect("record uncertain outcome");
        assert_eq!(reopened.pending(), Some(uncertain));
        assert!(matches!(
            reopened.finish(uncertain, FileOutcome::Failed),
            Err(FileAuditError::WrongOperation)
        ));
        reopened
            .acknowledge(uncertain)
            .expect("acknowledge uncertainty");
        assert_eq!(reopened.pending(), None);
    }

    #[test]
    fn failed_outcome_clears_pending_without_retry() {
        let directory = tempfile::tempdir().expect("temporary data directory");
        let (session_id, workspace_id) = scope();
        let mut audit = FileAudit::open(directory.path(), Uuid::new_v4()).expect("open audit");
        let operation_id = audit
            .start(
                session_id,
                workspace_id,
                0,
                FileOperation::List,
                native_target(Path::new("")),
            )
            .expect("record root listing intent");
        audit
            .finish(operation_id, FileOutcome::Failed)
            .expect("record failure");
        assert_eq!(audit.pending(), None);
        assert_eq!(audit.entries(10).expect("entries").len(), 3);
    }

    #[test]
    fn rejects_invalid_native_targets_before_writing() {
        let directory = tempfile::tempdir().expect("temporary data directory");
        let (session_id, workspace_id) = scope();
        let mut audit = FileAudit::open(directory.path(), Uuid::new_v4()).expect("open audit");
        let initial_bytes = audit.bytes;
        for path in ["", "../escape", "a/../b"] {
            assert!(matches!(
                audit.start(
                    session_id,
                    workspace_id,
                    0,
                    FileOperation::Read,
                    native_target(Path::new(path))
                ),
                Err(FileAuditError::Invalid(_))
            ));
        }
        assert_eq!(audit.bytes, initial_bytes);
        assert_eq!(audit.entries(10).expect("entries").len(), 1);
    }

    #[test]
    fn rejects_empty_corrupt_and_duplicate_key_files_without_repair() {
        let store_id = Uuid::new_v4();
        for contents in [
            Vec::new(),
            b"{\"bad\":true}\n".to_vec(),
            b"{\"a\":1,\"a\":2}\n".to_vec(),
        ] {
            let directory = tempfile::tempdir().expect("temporary data directory");
            let path = directory.path().join(FILE_NAME);
            fs::write(&path, &contents).expect("write malformed prior state");
            assert!(FileAudit::open(directory.path(), store_id).is_err());
            assert_eq!(fs::read(&path).expect("read preserved state"), contents);
        }
    }

    #[test]
    fn rejects_tampered_sequence_duplicate_ids_and_unknown_fields_on_replay() {
        let directory = tempfile::tempdir().expect("temporary data directory");
        let store_id = Uuid::new_v4();
        let (session_id, workspace_id) = scope();
        let mut audit = FileAudit::open(directory.path(), store_id).expect("open audit");
        let operation_id = audit
            .start(
                session_id,
                workspace_id,
                4,
                FileOperation::Read,
                valid_target(),
            )
            .expect("start operation");
        audit
            .finish(operation_id, FileOutcome::Succeeded)
            .expect("finish");
        let path = directory.path().join(FILE_NAME);
        drop(audit);
        let original = fs::read(&path).expect("read log");

        let sequence_tampered = original
            .windows(b"\"sequence\":1".len())
            .position(|window| window == b"\"sequence\":1")
            .map(|position| {
                let mut changed = original.clone();
                changed[position + b"\"sequence\":".len()] = b'9';
                changed
            })
            .expect("find sequence");
        let duplicate_event_id = {
            let mut values: Vec<serde_json::Value> = original
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_slice(line).expect("valid source event"))
                .collect();
            values[2]["event_id"] = values[1]["event_id"].clone();
            let mut changed = values
                .iter()
                .map(serde_json::to_vec)
                .collect::<Result<Vec<_>, _>>()
                .expect("encode events")
                .join(&b'\n');
            changed.push(b'\n');
            changed
        };
        let unknown_field = {
            let needle = b"\"schema_version\":1";
            let position = original
                .windows(needle.len())
                .position(|window| window == needle)
                .expect("find schema version");
            let mut changed = original[..position + needle.len()].to_vec();
            changed.extend_from_slice(b",\"unexpected\":true");
            changed.extend_from_slice(&original[position + needle.len()..]);
            changed
        };
        for tampered in [sequence_tampered, duplicate_event_id, unknown_field] {
            fs::write(&path, &tampered).expect("tamper log");
            assert!(matches!(
                FileAudit::open(directory.path(), store_id),
                Err(FileAuditError::Invalid(_))
            ));
            assert_eq!(fs::read(&path).expect("preserved corrupt log"), tampered);
        }
    }

    #[test]
    fn rejects_reused_operation_identifiers_on_replay() {
        let directory = tempfile::tempdir().expect("temporary data directory");
        let store_id = Uuid::new_v4();
        let (session_id, workspace_id) = scope();
        let mut audit = FileAudit::open(directory.path(), store_id).expect("open audit");
        let first = audit
            .start(
                session_id,
                workspace_id,
                1,
                FileOperation::Read,
                valid_target(),
            )
            .expect("first start");
        audit
            .finish(first, FileOutcome::Succeeded)
            .expect("first finish");
        let second = audit
            .start(
                session_id,
                workspace_id,
                2,
                FileOperation::Read,
                valid_target(),
            )
            .expect("second start");
        audit
            .finish(second, FileOutcome::Succeeded)
            .expect("second finish");
        let path = directory.path().join(FILE_NAME);
        drop(audit);

        let original = fs::read(&path).expect("read valid log");
        let mut values: Vec<serde_json::Value> = original
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).expect("valid source event"))
            .collect();
        values[3]["action"]["payload"]["operation_id"] =
            values[1]["action"]["payload"]["operation_id"].clone();
        let mut changed = values
            .iter()
            .map(serde_json::to_vec)
            .collect::<Result<Vec<_>, _>>()
            .expect("encode events")
            .join(&b'\n');
        changed.push(b'\n');
        fs::write(&path, &changed).expect("write duplicate operation identifier");

        assert!(matches!(
            FileAudit::open(directory.path(), store_id),
            Err(FileAuditError::Invalid(_))
        ));
        assert_eq!(
            fs::read(&path).expect("read preserved invalid log"),
            changed
        );
    }

    #[test]
    fn locks_are_exclusive_and_released_on_drop() {
        let directory = tempfile::tempdir().expect("temporary data directory");
        let store_id = Uuid::new_v4();
        let audit = FileAudit::open(directory.path(), store_id).expect("first writer");
        assert!(matches!(
            FileAudit::open(directory.path(), store_id),
            Err(FileAuditError::Busy)
        ));
        drop(audit);
        FileAudit::open(directory.path(), store_id).expect("lock released");
    }

    #[test]
    fn start_reserves_finish_and_ack_capacity() {
        let directory = tempfile::tempdir().expect("temporary data directory");
        let store_id = Uuid::new_v4();
        let (session_id, workspace_id) = scope();
        let mut audit = FileAudit::open(directory.path(), store_id).expect("open audit");
        let id = audit
            .start(
                session_id,
                workspace_id,
                0,
                FileOperation::Read,
                valid_target(),
            )
            .expect("start with reserved capacity");
        let encoded_start = audit.bytes;
        audit
            .finish(id, FileOutcome::Uncertain)
            .expect("reserved finish");
        audit.acknowledge(id).expect("reserved acknowledgement");
        assert!(audit.bytes > encoded_start);
        assert_eq!(audit.pending(), None);

        let second_directory = tempfile::tempdir().expect("second data directory");
        let mut capped =
            FileAudit::open(second_directory.path(), Uuid::new_v4()).expect("second audit");
        capped.bytes = MAX_BYTES - COMPLETION_AND_ACK_RESERVE_BYTES + 1;
        assert!(matches!(
            capped.start(
                session_id,
                workspace_id,
                0,
                FileOperation::Read,
                valid_target()
            ),
            Err(FileAuditError::LimitReached)
        ));
        capped.bytes = 0;
        let template = capped.entries[0].clone();
        capped.entries.resize(MAX_EVENTS - 2, template);
        assert!(matches!(
            capped.start(
                session_id,
                workspace_id,
                0,
                FileOperation::Read,
                valid_target()
            ),
            Err(FileAuditError::LimitReached)
        ));
    }

    #[test]
    fn failed_write_or_sync_poisons_writer_without_publishing_candidate_state() {
        for fail_sync in [false, true] {
            let directory = tempfile::tempdir().expect("temporary data directory");
            let (session_id, workspace_id) = scope();
            let mut audit = FileAudit::open(directory.path(), Uuid::new_v4()).expect("open audit");
            let initial_len = audit.entries.len();
            if fail_sync {
                audit.fail_next_sync_for_test();
            } else {
                audit.fail_next_write = true;
            }
            assert!(matches!(
                audit.start(
                    session_id,
                    workspace_id,
                    0,
                    FileOperation::Read,
                    valid_target()
                ),
                Err(FileAuditError::Io(_))
            ));
            assert_eq!(audit.entries.len(), initial_len);
            assert_eq!(audit.pending(), None);
            assert!(matches!(
                audit.start(
                    session_id,
                    workspace_id,
                    0,
                    FileOperation::Read,
                    valid_target()
                ),
                Err(FileAuditError::Poisoned)
            ));
        }
    }

    #[test]
    fn rejects_invalid_entry_limits() {
        let directory = tempfile::tempdir().expect("temporary data directory");
        let audit = FileAudit::open(directory.path(), Uuid::new_v4()).expect("open audit");
        assert!(matches!(audit.entries(0), Err(FileAuditError::Invalid(_))));
        assert!(matches!(
            audit.entries(101),
            Err(FileAuditError::Invalid(_))
        ));
    }

    #[test]
    fn operation_revisions_cannot_move_backwards() {
        let directory = tempfile::tempdir().expect("temporary data directory");
        let (session_id, workspace_id) = scope();
        let mut audit = FileAudit::open(directory.path(), Uuid::new_v4()).expect("open audit");
        let first = audit
            .start(
                session_id,
                workspace_id,
                5,
                FileOperation::Read,
                valid_target(),
            )
            .expect("start first operation");
        audit
            .finish(first, FileOutcome::Succeeded)
            .expect("finish first operation");
        let bytes = audit.bytes;
        assert!(matches!(
            audit.start(
                session_id,
                workspace_id,
                4,
                FileOperation::Read,
                valid_target()
            ),
            Err(FileAuditError::Invalid(_))
        ));
        assert_eq!(audit.bytes, bytes);
    }
}
