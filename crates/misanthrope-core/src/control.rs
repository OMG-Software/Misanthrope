//! Trusted local settings and audit journal. No method executes a workspace tool.
//!
//! Callers must keep the application data directory outside development roots.
//! Settings are user input, never model tools. Directory checks here detect
//! ordinary replacement and configuration mistakes; they are not an I/O broker.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};
use uuid::{Uuid, Version};

use crate::policy::{
    self, DecisionReason, Operation, PermissionSet, PolicyDecision, PolicyRequest, SessionBinding,
    WorkspaceId,
};
use crate::storage::{self, StorageError};
use crate::workspace::DirectoryRecord;

const FILE_NAME: &str = "control-v1.jsonl";
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 64 * 1024;
const MAX_EVENTS: usize = 4096;
const MAX_WORKSPACES: usize = 256;

/// A settings, validation, locking, or durability failure.
#[derive(Debug, Error)]
pub enum ControlError {
    /// Preserve the operating-system cause without treating it as absence.
    #[error("control journal I/O failed: {0}")]
    Io(#[from] io::Error),
    /// Existing state is invalid. It is never repaired or reset automatically.
    #[error("invalid control journal: {0}")]
    Invalid(&'static str),
    /// Another caller owns the journal lock.
    #[error("control journal is already open")]
    Busy,
    /// The caller used an obsolete settings revision.
    #[error("settings revision conflict: expected {expected}, current {actual}")]
    Conflict { expected: u64, actual: u64 },
    /// Configuration requires an explicit development root.
    #[error("development root has not been configured")]
    NoRoot,
    /// A requested workspace is not registered.
    #[error("workspace is not registered")]
    UnknownWorkspace,
    /// The selected directory is unavailable, replaced, or outside the root.
    #[error("workspace directory is unavailable or outside the configured root")]
    DirectoryChanged,
    /// Application storage must not be visible within a development root.
    #[error("development root and application data directories must not overlap")]
    StorageOverlap,
    /// A bounded settings or journal capacity has been reached.
    #[error("control journal capacity reached")]
    LimitReached,
    /// A previous write or sync had an uncertain outcome.
    #[error("control journal writer is poisoned")]
    Poisoned,
}

impl From<StorageError> for ControlError {
    fn from(value: StorageError) -> Self {
        match value {
            StorageError::Io(error) => Self::Io(error),
            StorageError::Invalid(reason) => Self::Invalid(reason),
            StorageError::Busy => Self::Busy,
        }
    }
}

/// Saved workspace identity and user preferences. This is not a capability.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRegistration {
    #[serde(deserialize_with = "deserialize_workspace_id")]
    id: WorkspaceId,
    directory: DirectoryRecord,
    permissions: PermissionSet,
}

impl WorkspaceRegistration {
    /// Stable project identity. Historical journal metadata cannot grant access.
    pub const fn id(&self) -> WorkspaceId {
        self.id
    }

    /// Current preferences, not an execution grant.
    pub const fn permissions(&self) -> PermissionSet {
        self.permissions
    }
}

fn deserialize_workspace_id<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<WorkspaceId, D::Error> {
    WorkspaceId::from_uuid(Uuid::deserialize(deserializer)?).map_err(serde::de::Error::custom)
}

/// A bounded, owned view of configured state. Paths retain their native encoding.
#[derive(Clone, Debug, Serialize)]
pub struct ControlSnapshot {
    store_id: Uuid,
    revision: u64,
    development_root: Option<DirectoryRecord>,
    workspaces: Vec<WorkspaceRegistration>,
    configuration_only: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Action {
    Created,
    RootSet {
        directory: DirectoryRecord,
    },
    WorkspaceRegistered {
        registration: WorkspaceRegistration,
    },
    PermissionsChanged {
        #[serde(deserialize_with = "deserialize_workspace_id")]
        workspace_id: WorkspaceId,
        permissions: PermissionSet,
    },
    PolicyChecked {
        binding_store_id: Uuid,
        binding_session_id: Uuid,
        request_session_id: Uuid,
        #[serde(deserialize_with = "deserialize_workspace_id")]
        binding_workspace_id: WorkspaceId,
        #[serde(deserialize_with = "deserialize_workspace_id")]
        target_workspace_id: WorkspaceId,
        binding_revision: u64,
        operation: Operation,
        reason: DecisionReason,
    },
}

/// One persisted audit event. It contains metadata, never credentials or prompts.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEntry {
    schema_version: u32,
    store_id: Uuid,
    event_id: Uuid,
    sequence: u64,
    timestamp: String,
    revision: u64,
    action: Action,
}

/// Single-writer settings authority for trusted local callers.
///
/// The lock lasts until drop. Every acknowledged mutation/check is synced. An
/// uncertain write poisons this object. It must not be held over provider I/O.
pub struct ControlStore {
    file: File,
    data_directory: DirectoryRecord,
    snapshot: ControlSnapshot,
    entries: Vec<AuditEntry>,
    bytes: usize,
    poisoned: bool,
    #[cfg(test)]
    fail_next_sync: bool,
}

impl ControlStore {
    /// Open or initialize `<data_directory>/control-v1.jsonl` under an exclusive lock.
    /// Existing empty, malformed, or incomplete files cause an error without repair.
    pub fn open(data_directory: &Path) -> Result<Self, ControlError> {
        storage::create_private_directories(data_directory)?;
        match storage::open_locked_file(&data_directory.join(FILE_NAME), true) {
            Ok(file) => {
                let mut store = Self::empty(file, data_directory)?;
                store.append(Action::Created, 0)?;
                storage::sync_parent_directory(data_directory)?;
                Ok(store)
            }
            Err(StorageError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists => {
                Self::open_existing(data_directory)?.ok_or_else(|| {
                    ControlError::Io(io::Error::new(
                        io::ErrorKind::NotFound,
                        "control journal disappeared",
                    ))
                })
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Open existing state without creating directories or files. Only absence is `None`.
    pub fn open_existing(data_directory: &Path) -> Result<Option<Self>, ControlError> {
        let mut file = match storage::open_locked_file(&data_directory.join(FILE_NAME), false) {
            Ok(file) => file,
            Err(StorageError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(u64::try_from(MAX_BYTES + 1).map_err(|_| ControlError::LimitReached)?)
            .read_to_end(&mut bytes)?;
        if bytes.is_empty() || bytes.len() > MAX_BYTES || !bytes.ends_with(b"\n") {
            return Err(ControlError::Invalid(
                "empty, oversized, or incomplete journal",
            ));
        }
        let mut store = Self::empty(file, data_directory)?;
        let mut event_ids = HashSet::new();
        let complete = bytes
            .strip_suffix(b"\n")
            .ok_or(ControlError::Invalid("incomplete journal"))?;
        for line in complete.split(|byte| *byte == b'\n') {
            if line.is_empty() || line.len() >= MAX_EVENT_BYTES || store.entries.len() >= MAX_EVENTS
            {
                return Err(ControlError::Invalid("invalid event size or count"));
            }
            let value = storage::parse_strict_json(line)
                .map_err(|_| ControlError::Invalid("invalid or duplicate JSON keys"))?;
            let entry: AuditEntry = serde_json::from_value(value)
                .map_err(|_| ControlError::Invalid("unsupported event shape"))?;
            if !event_ids.insert(entry.event_id) {
                return Err(ControlError::Invalid("duplicate event identifier"));
            }
            if store.entries.is_empty() {
                store.snapshot.store_id = entry.store_id;
            }
            Self::apply(&mut store.snapshot, &entry, store.entries.len())?;
            store.entries.push(entry);
        }
        store.bytes = bytes.len();
        Ok(Some(store))
    }

    fn empty(file: File, data_directory: &Path) -> Result<Self, ControlError> {
        Ok(Self {
            file,
            data_directory: DirectoryRecord::capture(data_directory)?,
            snapshot: ControlSnapshot {
                store_id: Uuid::new_v4(),
                revision: 0,
                development_root: None,
                workspaces: Vec::new(),
                configuration_only: true,
            },
            entries: Vec::new(),
            bytes: 0,
            poisoned: false,
            #[cfg(test)]
            fail_next_sync: false,
        })
    }

    /// Return saved state without granting access to its paths.
    pub fn snapshot(&self) -> ControlSnapshot {
        self.snapshot.clone()
    }
    /// Current global configuration revision. Audit-only checks do not change it.
    pub const fn revision(&self) -> u64 {
        self.snapshot.revision
    }
    /// Whether the user has explicitly selected a development root.
    pub const fn has_development_root(&self) -> bool {
        self.snapshot.development_root.is_some()
    }
    /// Look up descriptive workspace metadata by its stable identifier.
    pub fn workspace(&self, id: WorkspaceId) -> Option<&WorkspaceRegistration> {
        self.snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)
    }

    /// Select an existing development root outside application storage.
    /// Root changes invalidate every session binding through the global revision.
    pub fn set_development_root(
        &mut self,
        path: &Path,
        expected_revision: u64,
    ) -> Result<(), ControlError> {
        self.check_revision(expected_revision)?;
        let directory = DirectoryRecord::capture(path)?;
        self.check_storage_overlap(&directory)?;
        if self.snapshot.development_root.as_ref() == Some(&directory) {
            return Ok(());
        }
        self.append(Action::RootSet { directory }, self.next_revision()?)
    }

    /// Register an existing directory beneath the current root, or reuse its ID.
    /// Replaced paths cannot inherit the previous directory's preferences.
    pub fn register_workspace(
        &mut self,
        path: &Path,
        expected_revision: u64,
    ) -> Result<WorkspaceRegistration, ControlError> {
        self.check_revision(expected_revision)?;
        let directory = DirectoryRecord::capture(path)?;
        self.check_directory_scope(&directory)?;
        for workspace in &self.snapshot.workspaces {
            if workspace.directory.same_directory(&directory) {
                return Ok(workspace.clone());
            }
            if workspace.directory.path == directory.path {
                return Err(ControlError::DirectoryChanged);
            }
        }
        let registration = WorkspaceRegistration {
            id: WorkspaceId::fresh(),
            directory,
            permissions: PermissionSet::default(),
        };
        self.append(
            Action::WorkspaceRegistered {
                registration: registration.clone(),
            },
            self.next_revision()?,
        )?;
        Ok(registration)
    }

    /// Change preferences with an explicit compare-and-set revision.
    pub fn set_permissions(
        &mut self,
        id: WorkspaceId,
        permissions: PermissionSet,
        expected_revision: u64,
    ) -> Result<(), ControlError> {
        self.check_revision(expected_revision)?;
        let workspace = self.workspace(id).ok_or(ControlError::UnknownWorkspace)?;
        if workspace.permissions == permissions {
            return Ok(());
        }
        self.append(
            Action::PermissionsChanged {
                workspace_id: id,
                permissions,
            },
            self.next_revision()?,
        )
    }

    /// Return at most 100 most recent audit records, in original order.
    pub fn audit(&self, limit: usize) -> Result<Vec<AuditEntry>, ControlError> {
        if !(1..=100).contains(&limit) {
            return Err(ControlError::Invalid("audit limit must be 1 through 100"));
        }
        Ok(self
            .entries
            .iter()
            .skip(self.entries.len().saturating_sub(limit))
            .cloned()
            .collect())
    }

    /// Create a process-local 15-minute binding after current directory checks.
    /// The caller must supply a trusted session selection, not a journal grant.
    pub fn bind_session(
        &self,
        session_id: Uuid,
        id: WorkspaceId,
    ) -> Result<SessionBinding, ControlError> {
        self.ensure_healthy()?;
        if !valid_uuid(session_id) {
            return Err(ControlError::Invalid("session ID must be version 4"));
        }
        let workspace = self.workspace(id).ok_or(ControlError::UnknownWorkspace)?;
        self.check_directory_scope(&workspace.directory)?;
        Ok(SessionBinding::new(
            self.snapshot.store_id,
            session_id,
            id,
            self.revision(),
        ))
    }

    /// Audit a configuration-only result before returning it. Execution is always denied.
    pub fn evaluate(
        &mut self,
        binding: &SessionBinding,
        request: &PolicyRequest,
    ) -> Result<PolicyDecision, ControlError> {
        self.ensure_healthy()?;
        if !valid_uuid(request.session_id) {
            return Err(ControlError::Invalid("session ID must be version 4"));
        }
        let permissions = self.workspace(request.workspace_id).and_then(|workspace| {
            self.check_directory_scope(&workspace.directory)
                .ok()
                .map(|()| workspace.permissions)
        });
        let decision = policy::evaluate(
            self.snapshot.store_id,
            self.revision(),
            permissions,
            binding,
            request,
        );
        self.append(
            Action::PolicyChecked {
                binding_store_id: binding.store_id(),
                binding_session_id: binding.session_id(),
                request_session_id: request.session_id,
                binding_workspace_id: binding.workspace_id(),
                target_workspace_id: request.workspace_id,
                binding_revision: binding.revision(),
                operation: request.operation,
                reason: decision.reason(),
            },
            self.revision(),
        )?;
        Ok(decision)
    }

    fn check_storage_overlap(&self, directory: &DirectoryRecord) -> Result<(), ControlError> {
        if directory.contains(&self.data_directory)? || self.data_directory.contains(directory)? {
            return Err(ControlError::StorageOverlap);
        }
        Ok(())
    }

    fn check_directory_scope(&self, directory: &DirectoryRecord) -> Result<(), ControlError> {
        let root = self
            .snapshot
            .development_root
            .as_ref()
            .ok_or(ControlError::NoRoot)?;
        self.check_storage_overlap(root)?;
        if !directory.is_current()? || !root.contains(directory)? {
            return Err(ControlError::DirectoryChanged);
        }
        Ok(())
    }

    fn ensure_healthy(&self) -> Result<(), ControlError> {
        if self.poisoned {
            Err(ControlError::Poisoned)
        } else {
            Ok(())
        }
    }
    fn check_revision(&self, expected: u64) -> Result<(), ControlError> {
        self.ensure_healthy()?;
        if expected != self.revision() {
            return Err(ControlError::Conflict {
                expected,
                actual: self.revision(),
            });
        }
        Ok(())
    }
    fn next_revision(&self) -> Result<u64, ControlError> {
        self.revision()
            .checked_add(1)
            .ok_or(ControlError::LimitReached)
    }

    /// Preflight the complete next state, then publish it only after durable append.
    fn append(&mut self, action: Action, revision: u64) -> Result<(), ControlError> {
        self.ensure_healthy()?;
        if self.entries.len() >= MAX_EVENTS {
            return Err(ControlError::LimitReached);
        }
        let entry = AuditEntry {
            schema_version: 1,
            store_id: self.snapshot.store_id,
            event_id: Uuid::new_v4(),
            sequence: u64::try_from(self.entries.len()).map_err(|_| ControlError::LimitReached)?,
            timestamp: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .map_err(|_| ControlError::Invalid("clock cannot be encoded"))?,
            revision,
            action,
        };
        let mut next = self.snapshot.clone();
        Self::apply(&mut next, &entry, self.entries.len())?;
        let mut encoded = serde_json::to_vec(&entry)
            .map_err(|_| ControlError::Invalid("event cannot be encoded"))?;
        encoded.push(b'\n');
        let bytes = self
            .bytes
            .checked_add(encoded.len())
            .ok_or(ControlError::LimitReached)?;
        if encoded.len() > MAX_EVENT_BYTES || bytes > MAX_BYTES {
            return Err(ControlError::LimitReached);
        }
        let result = self
            .file
            .seek(SeekFrom::End(0))
            .and_then(|_| self.file.write_all(&encoded))
            .and_then(|()| self.sync());
        if let Err(error) = result {
            self.poisoned = true;
            return Err(ControlError::Io(error));
        }
        self.snapshot = next;
        self.entries.push(entry);
        self.bytes = bytes;
        Ok(())
    }

    fn sync(&self) -> io::Result<()> {
        #[cfg(test)]
        if self.fail_next_sync {
            return Err(io::Error::other("injected control sync failure"));
        }
        self.file.sync_all()
    }

    fn apply(
        state: &mut ControlSnapshot,
        entry: &AuditEntry,
        count: usize,
    ) -> Result<(), ControlError> {
        let timestamp = OffsetDateTime::parse(&entry.timestamp, &Rfc3339)
            .map_err(|_| ControlError::Invalid("invalid timestamp"))?;
        if entry.schema_version != 1
            || !valid_uuid(entry.store_id)
            || !valid_uuid(entry.event_id)
            || entry.store_id != state.store_id
            || entry.sequence != u64::try_from(count).map_err(|_| ControlError::LimitReached)?
            || timestamp.offset() != UtcOffset::UTC
        {
            return Err(ControlError::Invalid("invalid envelope or sequence"));
        }
        let configuration_change =
            !matches!(entry.action, Action::Created | Action::PolicyChecked { .. });
        let revision = if configuration_change {
            state
                .revision
                .checked_add(1)
                .ok_or(ControlError::LimitReached)?
        } else {
            state.revision
        };
        if entry.revision != revision || (count == 0) != matches!(entry.action, Action::Created) {
            return Err(ControlError::Invalid("invalid revision or initial event"));
        }
        match &entry.action {
            Action::Created => {}
            Action::RootSet { directory } => {
                if !directory.validate_shape() {
                    return Err(ControlError::Invalid("invalid root directory"));
                }
                state.development_root = Some(directory.clone());
            }
            Action::WorkspaceRegistered { registration } => {
                if state.development_root.is_none()
                    || state.workspaces.len() >= MAX_WORKSPACES
                    || !registration.directory.validate_shape()
                    || registration.permissions != PermissionSet::default()
                    || state.workspaces.iter().any(|existing| {
                        existing.id == registration.id
                            || existing.directory.path == registration.directory.path
                            || existing.directory.same_directory(&registration.directory)
                    })
                {
                    return Err(ControlError::Invalid(
                        "invalid or duplicate workspace registration",
                    ));
                }
                state.workspaces.push(registration.clone());
            }
            Action::PermissionsChanged {
                workspace_id,
                permissions,
            } => {
                let workspace = state
                    .workspaces
                    .iter_mut()
                    .find(|workspace| workspace.id == *workspace_id)
                    .ok_or(ControlError::Invalid("unknown permission workspace"))?;
                workspace.permissions = *permissions;
            }
            Action::PolicyChecked {
                binding_store_id,
                binding_session_id,
                request_session_id,
                binding_workspace_id,
                target_workspace_id,
                binding_revision,
                operation,
                reason,
            } => {
                if !valid_uuid(*binding_store_id)
                    || !valid_uuid(*binding_session_id)
                    || !valid_uuid(*request_session_id)
                {
                    return Err(ControlError::Invalid("invalid audit identifiers"));
                }
                // Only enabled results assert a positive policy fact. Expiry and
                // filesystem availability cannot be reconstructed during replay.
                if *reason == DecisionReason::Enabled
                    && (*binding_store_id != state.store_id
                        || binding_session_id != request_session_id
                        || binding_workspace_id != target_workspace_id
                        || *binding_revision != state.revision
                        || !state.workspaces.iter().any(|workspace| {
                            workspace.id == *target_workspace_id
                                && workspace.permissions.allows(*operation)
                        }))
                {
                    return Err(ControlError::Invalid("inconsistent enabled policy audit"));
                }
            }
        }
        state.revision = revision;
        Ok(())
    }
}

fn valid_uuid(value: Uuid) -> bool {
    !value.is_nil() && value.get_version() == Some(Version::Random)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn persisted_bytes(store: &mut ControlStore) -> Vec<u8> {
        store
            .file
            .seek(SeekFrom::Start(0))
            .expect("seek owned handle");
        let mut bytes = Vec::new();
        store
            .file
            .read_to_end(&mut bytes)
            .expect("read owned handle");
        bytes
    }

    #[test]
    fn write_failure_preserves_state_and_poisons_future_settings() {
        let directory = tempfile::tempdir().expect("private temporary directory");
        let data = directory.path().join("data");
        let root = directory.path().join("projects");
        fs::create_dir(&root).expect("development root");
        let mut store = ControlStore::open(&data).expect("control store");
        let before = persisted_bytes(&mut store);
        store.file = File::open(data.join(FILE_NAME)).expect("read-only fault handle");

        assert!(matches!(
            store.set_development_root(&root, 0),
            Err(ControlError::Io(_))
        ));
        assert_eq!(store.revision(), 0);
        assert!(!store.has_development_root());
        assert_eq!(
            fs::read(data.join(FILE_NAME)).expect("unchanged bytes"),
            before
        );
        assert!(matches!(
            store.set_development_root(&root, 0),
            Err(ControlError::Poisoned)
        ));
    }

    #[test]
    fn failed_audit_sync_returns_no_decision_and_poisons_writer() {
        let directory = tempfile::tempdir().expect("private temporary directory");
        let data = directory.path().join("data");
        let root = directory.path().join("projects");
        fs::create_dir(&root).expect("development root");
        let mut store = ControlStore::open(&data).expect("control store");
        store.set_development_root(&root, 0).expect("root setting");
        let workspace = store.register_workspace(&root, 1).expect("workspace");
        let session = Uuid::new_v4();
        let binding = store
            .bind_session(session, workspace.id())
            .expect("live binding");
        let request = PolicyRequest {
            session_id: session,
            workspace_id: workspace.id(),
            operation: Operation::Read,
        };
        let before_bytes = store.file.metadata().expect("file metadata").len();
        let before_audit = store.audit(100).expect("audit").len();
        store.fail_next_sync = true;

        assert!(matches!(
            store.evaluate(&binding, &request),
            Err(ControlError::Io(_))
        ));
        assert!(store.file.metadata().expect("metadata after write").len() > before_bytes);
        assert_eq!(store.revision(), 2);
        assert_eq!(
            store.audit(100).expect("unacknowledged event absent").len(),
            before_audit
        );
        assert!(matches!(
            store.evaluate(&binding, &request),
            Err(ControlError::Poisoned)
        ));
        assert!(matches!(
            store.bind_session(session, workspace.id()),
            Err(ControlError::Poisoned)
        ));
    }

    #[test]
    fn replay_rejects_sequence_revision_and_duplicate_event_ids_without_changes() {
        for corruption in ["sequence", "revision", "event_id", "store_id", "timestamp"] {
            let directory = tempfile::tempdir().expect("private temporary directory");
            let data = directory.path().join("data");
            let root = directory.path().join("root");
            fs::create_dir(&root).expect("root");
            let mut store = ControlStore::open(&data).expect("store");
            store.set_development_root(&root, 0).expect("root setting");
            let mut entries = store.entries.clone();
            drop(store);
            let first_id = entries.first().expect("created event").event_id;
            let second = entries.get_mut(1).expect("root event");
            match corruption {
                "sequence" => second.sequence = 4,
                "revision" => second.revision = 0,
                "event_id" => second.event_id = first_id,
                "store_id" => second.store_id = Uuid::new_v4(),
                "timestamp" => second.timestamp = "invalid".into(),
                _ => panic!("fixed test case"),
            }
            let mut bytes = Vec::new();
            for entry in entries {
                bytes.extend(serde_json::to_vec(&entry).expect("event JSON"));
                bytes.push(b'\n');
            }
            fs::write(data.join(FILE_NAME), &bytes).expect("corrupted fixture");
            assert!(ControlStore::open(&data).is_err(), "{corruption}");
            assert_eq!(
                fs::read(data.join(FILE_NAME)).expect("fixture preserved"),
                bytes
            );
        }
    }

    #[test]
    fn byte_event_and_audit_limits_fail_without_mutating_state() {
        let directory = tempfile::tempdir().expect("private temporary directory");
        let data = directory.path().join("data");
        let root = directory.path().join("root");
        fs::create_dir(&root).expect("root");
        let mut store = ControlStore::open(&data).expect("store");
        let before = persisted_bytes(&mut store);
        assert!(store.audit(0).is_err());
        assert!(store.audit(101).is_err());
        store.bytes = MAX_BYTES;
        assert!(matches!(
            store.set_development_root(&root, 0),
            Err(ControlError::LimitReached)
        ));
        assert_eq!(store.revision(), 0);
        assert_eq!(persisted_bytes(&mut store), before);
        store.bytes = before.len();
        let initial = store.entries.first().expect("initial event").clone();
        store.entries.resize(MAX_EVENTS, initial);
        assert!(matches!(
            store.set_development_root(&root, 0),
            Err(ControlError::LimitReached)
        ));
        assert_eq!(persisted_bytes(&mut store), before);
    }
}
