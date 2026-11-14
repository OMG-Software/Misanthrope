//! Trusted local settings and audit journal. No method executes a workspace tool.
//!
//! Callers must keep the application data directory outside development roots.
//! Settings are user input, never model tools. Directory checks here detect
//! ordinary replacement and configuration mistakes; they are not an I/O broker.

use std::collections::{BTreeMap, HashSet};
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

mod approval;
mod container;
mod package;
mod profile;
mod provider;

pub use approval::{
    ApprovalBinding, ApprovalDecision, ApprovalDeclaration, ApprovalOperation, ApprovalRequest,
    ApprovalState, ApprovalStatus, MAX_APPROVAL_SUMMARY_BYTES, MAX_APPROVAL_TTL_SECS,
    MAX_APPROVALS, MAX_PENDING_APPROVALS, MIN_APPROVAL_TTL_SECS, now_unix,
};

pub use container::{
    ContainerGrant, ContainerGrantAccess, ContainerGrantState, ContainerGrantStatus,
    ContainerRevocation, container_name,
};
/// Whether this platform can record and apply container grants.
pub use omgcode_platform::container::CONTAINER_GRANTS_SUPPORTED;
pub use package::{
    MAX_PACKAGE_ORIGIN_BYTES, MAX_PACKAGE_ORIGINS, MAX_PACKAGE_OUTPUT_BYTES, MAX_PACKAGE_PROFILES,
    PackageFetchFailure, PackageFetchFinish, PackageFetchOutcome, PackageFetchStart, PackageOrigin,
    PackageProfile, PackageProfileDeclaration, PackageProfileStatus, PackageScheme, parse_origin,
    valid_output_name,
};
pub use profile::{
    MAX_PROFILE_NAME_CHARS, MAX_PROFILE_PATH_ENTRIES, MAX_PROFILE_ROOTS, MAX_PROFILE_VARIABLES,
    MAX_PROFILES, MAX_VARIABLE_NAME_BYTES, MAX_VARIABLE_VALUE_BYTES, ProfileDeclaration,
    ProfilePlatform, ProfileStatus, ProfileVariable, RESERVED_VARIABLES, ToolchainProfile,
    is_reserved_variable, valid_profile_name,
};
pub use provider::{
    MAX_PROVIDER_PROFILES, ProviderAuth, ProviderKind, ProviderProfile, ProviderProfileDeclaration,
    ProviderProfileStatus,
};

const FILE_NAME: &str = "control-v1.jsonl";
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 64 * 1024;
const MAX_EVENTS: usize = 4096;
const MAX_WORKSPACES: usize = 256;
// Permission events have fixed IDs, timestamps, and four boolean preferences.
// Leave a conservative budget for that bounded record before admitting file I/O.
const PERMISSION_EVENT_RESERVE_BYTES: usize = 2048;

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
    /// The workspace name beneath the root cannot be used by the bounded broker.
    #[error("workspace path beneath the development root is unsupported by the file broker: {0}")]
    WorkspacePath(#[source] io::Error),
    /// A bounded settings or journal capacity has been reached.
    #[error("control journal capacity reached")]
    LimitReached,
    /// A previous write or sync had an uncertain outcome.
    #[error("control journal writer is poisoned")]
    Poisoned,
    /// A container grant or revocation could not be applied or confirmed.
    /// The journal keeps the grant visible and revocable.
    #[error("container access change failed: {0}")]
    Container(#[source] io::Error),
    /// The last grant was revoked and recorded, but the container profile
    /// could not be deleted.
    #[error("the grant was revoked, but the container profile could not be deleted: {0}")]
    ProfileRemoval(#[source] io::Error),
    /// No recorded container grant has this identifier.
    #[error("container grant is not recorded")]
    UnknownGrant,
    /// The requested container grant is outside the allowed scope.
    #[error("container grant refused: {0}")]
    GrantRefused(&'static str),
    /// No recorded toolchain profile has this name.
    #[error("toolchain profile is not recorded")]
    UnknownProfile,
    /// The requested toolchain profile is invalid or outside the allowed scope.
    #[error("toolchain profile refused: {0}")]
    ProfileRefused(&'static str),
    /// No recorded package-network profile has this name.
    #[error("package-network profile is not recorded")]
    UnknownPackageProfile,
    /// The requested package-network profile is invalid.
    #[error("package-network profile refused: {0}")]
    PackageProfileRefused(&'static str),
    /// The requested package cache root is invalid or overlaps protected data.
    #[error("package cache root refused: {0}")]
    PackageCacheRefused(&'static str),
    /// No recorded provider profile has this name.
    #[error("provider profile is not recorded")]
    UnknownProvider,
    /// The requested provider profile is invalid.
    #[error("provider profile refused: {0}")]
    ProviderRefused(&'static str),
    /// No retained approval has this identifier.
    #[error("approval is not recorded")]
    UnknownApproval,
    /// The approval request, decision, or consumption is not valid.
    #[error("approval refused: {0}")]
    ApprovalRefused(&'static str),
}

impl From<StorageError> for ControlError {
    /// Preserve I/O causes and lock conflicts when crossing the storage boundary.
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

/// Reject malformed persisted IDs before constructing typed workspace metadata.
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
    #[serde(serialize_with = "serialize_workspace_views")]
    workspaces: Vec<WorkspaceRegistration>,
    configuration_only: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    container_grants: Vec<ContainerGrantStatus>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    profiles: Vec<ProfileStatus>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    package_profiles: Vec<PackageProfileStatus>,
    /// Operator-selected fetched-package roots, one per workspace, outside
    /// application storage and the development root.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    package_cache_roots: Vec<PackageCacheRootStatus>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    provider_profiles: Vec<ProviderProfileStatus>,
    /// Trusted per-call approval records. Operational state, not settings.
    #[serde(skip)]
    approvals: Vec<approval::ApprovalRecord>,
    /// Container revocations requested per workspace since the journal was
    /// created. Derived on replay and never serialized; run watchers compare
    /// it with the count when their run started.
    #[serde(skip)]
    grant_revocations: BTreeMap<WorkspaceId, u64>,
}

/// One workspace's selected fetched-package cache root.
#[derive(Clone, Debug, Serialize)]
pub struct PackageCacheRootStatus {
    workspace_id: WorkspaceId,
    directory: DirectoryRecord,
    revision: u64,
}

/// Operator-facing workspace view that reports every preference explicitly.
///
/// Persisted records omit a disabled `model_tools` flag for compatibility;
/// status output must still show that it is disabled.
#[derive(Serialize)]
struct WorkspaceView<'a> {
    id: WorkspaceId,
    directory: &'a DirectoryRecord,
    permissions: PermissionView,
}

/// All preference flags, including disabled optional ones.
#[derive(Serialize)]
struct PermissionView {
    read: bool,
    write: bool,
    execute: bool,
    task_network: bool,
    model_tools: bool,
    host_services: bool,
}

fn serialize_workspace_views<S: serde::Serializer>(
    workspaces: &[WorkspaceRegistration],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(workspaces.iter().map(|workspace| {
        let permissions = workspace.permissions;
        WorkspaceView {
            id: workspace.id,
            directory: &workspace.directory,
            permissions: PermissionView {
                read: permissions.read,
                write: permissions.write,
                execute: permissions.execute,
                task_network: permissions.task_network,
                model_tools: permissions.model_tools,
                host_services: permissions.host_services,
            },
        }
    }))
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
    ContainerGrantRequested {
        grant: ContainerGrant,
    },
    ContainerGrantApplied {
        grant_id: Uuid,
    },
    ContainerRevokeRequested {
        grant_id: Uuid,
    },
    ContainerGrantRevoked {
        grant_id: Uuid,
    },
    ProfileAdded {
        profile: ToolchainProfile,
    },
    ProfileRemoved {
        profile_id: Uuid,
    },
    PackageProfileAdded {
        profile: PackageProfile,
    },
    PackageProfileRemoved {
        profile_id: Uuid,
    },
    PackageFetchStarted {
        start: PackageFetchStart,
    },
    PackageFetchFinished {
        finish: PackageFetchFinish,
    },
    PackageCacheRootSet {
        #[serde(deserialize_with = "deserialize_workspace_id")]
        workspace_id: WorkspaceId,
        directory: DirectoryRecord,
    },
    PackageCacheRootCleared {
        #[serde(deserialize_with = "deserialize_workspace_id")]
        workspace_id: WorkspaceId,
    },
    ProviderProfileAdded {
        profile: ProviderProfile,
    },
    ProviderProfileRemoved {
        profile_id: Uuid,
    },
    ApprovalRequested {
        request: ApprovalRequest,
    },
    ApprovalDecided {
        approval_id: Uuid,
        decision: ApprovalDecision,
    },
    ApprovalConsumed {
        approval_id: Uuid,
    },
}

/// One persisted settings/audit event, never prompts or dedicated credential fields.
/// Native path metadata can itself contain secrets and must remain private.
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

/// Descriptive directory records. Only the broker's live checks can authorize I/O.
pub(crate) struct FileScope {
    pub(crate) root: DirectoryRecord,
    pub(crate) workspace: DirectoryRecord,
}

impl ControlStore {
    /// Get current records after configuration checks; this does not mint a permit.
    pub(crate) fn file_scope(&self, workspace_id: WorkspaceId) -> Result<FileScope, ControlError> {
        self.ensure_healthy()?;
        let workspace = self
            .workspace(workspace_id)
            .ok_or(ControlError::UnknownWorkspace)?;
        self.check_directory_scope(&workspace.directory)?;
        self.check_broker_scope_path(&workspace.directory)?;
        let root = self
            .snapshot
            .development_root
            .as_ref()
            .ok_or(ControlError::NoRoot)?;
        Ok(FileScope {
            root: root.clone(),
            workspace: workspace.directory.clone(),
        })
    }

    /// Locate private operation audit storage without exposing it as a task target.
    pub(crate) fn file_audit_context(&self) -> Result<(std::path::PathBuf, Uuid), ControlError> {
        self.ensure_healthy()?;
        if !self.data_directory.is_current()? {
            return Err(ControlError::DirectoryChanged);
        }
        Ok((self.data_directory.live_path()?, self.snapshot.store_id))
    }

    /// Supply expected private-root identity for comparison with the actual
    /// opened handle. This metadata grants no workspace or storage authority.
    pub(crate) fn replacement_context(&self) -> Result<(DirectoryRecord, Uuid), ControlError> {
        self.file_audit_context()?;
        Ok((self.data_directory.clone(), self.snapshot.store_id))
    }

    /// The control journal path inside `data_directory`. Its length changes
    /// with every recorded settings change.
    pub(crate) fn journal_path(data_directory: &Path) -> std::path::PathBuf {
        data_directory.join(FILE_NAME)
    }

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
        let path = data_directory.join(FILE_NAME);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.is_file() => {
                return Err(ControlError::Invalid("control path is not a regular file"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(ControlError::Io(error)),
        }
        // Existing state that disappears or fails validation must not become
        // an implicit legacy-mode selection. Handle checks still follow below.
        let mut file = storage::open_locked_file(&path, false)?;
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

    /// Capture trusted storage identity before initialization or validated replay.
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
                container_grants: Vec::new(),
                profiles: Vec::new(),
                package_profiles: Vec::new(),
                package_cache_roots: Vec::new(),
                provider_profiles: Vec::new(),
                approvals: Vec::new(),
                grant_revocations: BTreeMap::new(),
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

    /// Select the fetched-package cache directory for one workspace. It must
    /// exist, must not overlap application storage, must not overlap the
    /// development root, and must not overlap another workspace's cache root.
    pub fn set_package_cache_root(
        &mut self,
        workspace_id: WorkspaceId,
        path: &Path,
        expected_revision: u64,
    ) -> Result<(), ControlError> {
        self.check_revision(expected_revision)?;
        if self.workspace(workspace_id).is_none() {
            return Err(ControlError::UnknownWorkspace);
        }
        let directory = DirectoryRecord::capture(path)?;
        self.check_storage_overlap(&directory)?;
        let development = self
            .snapshot
            .development_root
            .as_ref()
            .ok_or(ControlError::NoRoot)?;
        if development.contains(&directory)? || directory.contains(development)? {
            return Err(ControlError::PackageCacheRefused(
                "the package cache root must not overlap the development root",
            ));
        }
        for existing in &self.snapshot.package_cache_roots {
            if existing.workspace_id != workspace_id
                && (existing.directory.contains(&directory)?
                    || directory.contains(&existing.directory)?)
            {
                return Err(ControlError::PackageCacheRefused(
                    "package cache roots must not overlap another workspace's root",
                ));
            }
        }
        if self.snapshot.package_cache_roots.iter().any(|existing| {
            existing.workspace_id == workspace_id && existing.directory.same_directory(&directory)
        }) {
            return Ok(());
        }
        self.append(
            Action::PackageCacheRootSet {
                workspace_id,
                directory,
            },
            self.next_revision()?,
        )?;
        Ok(())
    }

    /// Clear one workspace's package cache root. This advances the settings
    /// revision only when a root was recorded. Cached files are left in place.
    pub fn clear_package_cache_root(
        &mut self,
        workspace_id: WorkspaceId,
        expected_revision: u64,
    ) -> Result<(), ControlError> {
        self.check_revision(expected_revision)?;
        if self.workspace(workspace_id).is_none() {
            return Err(ControlError::UnknownWorkspace);
        }
        if !self
            .snapshot
            .package_cache_roots
            .iter()
            .any(|existing| existing.workspace_id == workspace_id)
        {
            return Ok(());
        }
        self.append(
            Action::PackageCacheRootCleared { workspace_id },
            self.next_revision()?,
        )?;
        Ok(())
    }

    /// The selected package cache root for one workspace, if any.
    pub(crate) fn package_cache_root(&self, workspace_id: WorkspaceId) -> Option<&DirectoryRecord> {
        self.snapshot
            .package_cache_roots
            .iter()
            .find(|status| status.workspace_id == workspace_id)
            .map(|status| &status.directory)
    }
    /// Look up descriptive workspace metadata by its stable identifier.
    pub fn workspace(&self, id: WorkspaceId) -> Option<&WorkspaceRegistration> {
        self.snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)
    }

    /// Find the registration whose stable ID equals an untrusted recorded UUID.
    ///
    /// A journal's recorded workspace UUID can only select an existing
    /// registration. The returned ID and preferences come from these settings,
    /// and the lookup grants nothing.
    pub fn registered_workspace(&self, recorded: Uuid) -> Option<&WorkspaceRegistration> {
        self.snapshot
            .workspaces
            .iter()
            .find(|workspace| *workspace.id.as_uuid() == recorded)
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
        self.check_broker_scope_path(&directory)?;
        for workspace in &self.snapshot.workspaces {
            if workspace.directory.same_directory(&directory) {
                if !workspace.directory.is_current()? {
                    return Err(ControlError::DirectoryChanged);
                }
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
        self.check_broker_scope_path(&workspace.directory)?;
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
        let decision = self.check_file_policy(binding, request)?;
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

    /// Check current preferences without consuming settings-journal capacity.
    ///
    /// Only the broker uses this operational check. It still needs native scope
    /// verification and a separate durable operation intent before target I/O.
    /// The public diagnostic `evaluate` command continues to append its audit.
    pub(crate) fn check_file_policy(
        &self,
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
        // A full settings log must not leave usable file authority that the
        // operator cannot revoke. Keep a conservative bounded permission-event
        // budget available before admitting target I/O; actual changes still
        // pass the ordinary exact append preflight.
        if decision.configured_allowed()
            && (self.entries.len() >= MAX_EVENTS
                || self
                    .bytes
                    .checked_add(PERMISSION_EVENT_RESERVE_BYTES)
                    .is_none_or(|size| size > MAX_BYTES))
        {
            return Err(ControlError::LimitReached);
        }
        Ok(decision)
    }

    /// Validate an explicitly selected archive directory outside development data.
    /// The caller holds this control writer until archival and directory sync end.
    pub(crate) fn file_archive_directory(
        &self,
        path: &Path,
    ) -> Result<std::path::PathBuf, ControlError> {
        self.file_audit_context()?;
        let directory = DirectoryRecord::capture(path)?;
        if self.data_directory.contains(&directory)? || directory.contains(&self.data_directory)? {
            return Err(ControlError::StorageOverlap);
        }
        if let Some(root) = &self.snapshot.development_root {
            if !root.is_current()? {
                return Err(ControlError::DirectoryChanged);
            }
            if root.contains(&directory)? || directory.contains(root)? {
                return Err(ControlError::StorageOverlap);
            }
        }
        directory.live_path().map_err(ControlError::Io)
    }

    /// Reject a scope the broker cannot traverse before it is registered or bound.
    fn check_broker_scope_path(&self, directory: &DirectoryRecord) -> Result<(), ControlError> {
        let root = self
            .snapshot
            .development_root
            .as_ref()
            .ok_or(ControlError::NoRoot)?;
        let root_path = root.live_path()?;
        let workspace_path = directory.live_path()?;
        let relative = workspace_path.strip_prefix(&root_path).map_err(|_| {
            ControlError::WorkspacePath(io::Error::new(
                io::ErrorKind::InvalidInput,
                "workspace name cannot be derived beneath root",
            ))
        })?;
        omgcode_platform::broker::RelativePath::new(relative)
            .map(|_| ())
            .map_err(ControlError::WorkspacePath)
    }

    /// Require a current storage record before interpreting negative ancestry checks.
    fn check_storage_overlap(&self, directory: &DirectoryRecord) -> Result<(), ControlError> {
        if !self.data_directory.is_current()? {
            return Err(ControlError::DirectoryChanged);
        }
        if directory.contains(&self.data_directory)? || self.data_directory.contains(directory)? {
            return Err(ControlError::StorageOverlap);
        }
        Ok(())
    }

    /// Recheck storage separation and current root membership without granting I/O.
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

    /// Prevent any further mutation or binding after an uncertain journal append.
    fn ensure_healthy(&self) -> Result<(), ControlError> {
        if self.poisoned {
            Err(ControlError::Poisoned)
        } else {
            Ok(())
        }
    }
    /// Reject stale trusted settings commands before filesystem checks or writes.
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
    /// Reserve a new configuration revision without allowing counter wraparound.
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
        // Keep room for every outstanding grant to reach a confirmed
        // revocation, so a full journal never strands container access.
        if !container::uses_grant_reserve(&entry.action) {
            let (events, reserve) = Self::grant_reserve(&next);
            let events_after = self
                .entries
                .len()
                .checked_add(1)
                .and_then(|count| count.checked_add(events));
            let bytes_after = bytes.checked_add(reserve);
            if events_after.is_none_or(|count| count > MAX_EVENTS)
                || bytes_after.is_none_or(|size| size > MAX_BYTES)
            {
                return Err(ControlError::LimitReached);
            }
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

    /// Sync appended bytes; tests can model failure after a successful write.
    fn sync(&self) -> io::Result<()> {
        #[cfg(test)]
        if self.fail_next_sync {
            return Err(io::Error::other("injected control sync failure"));
        }
        self.file.sync_all()
    }

    /// Validate one delta against the preceding state, for replay and append preflight.
    /// Filesystem observations and expired bindings are never restored from this log.
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
        // Grant outcomes and revocations remove or confirm authority that a
        // revisioned request already recorded, so they keep the revision.
        let configuration_change = !matches!(
            entry.action,
            Action::Created
                | Action::PolicyChecked { .. }
                | Action::ContainerGrantApplied { .. }
                | Action::ContainerRevokeRequested { .. }
                | Action::ContainerGrantRevoked { .. }
                | Action::PackageFetchStarted { .. }
                | Action::PackageFetchFinished { .. }
                | Action::ApprovalRequested { .. }
                | Action::ApprovalDecided { .. }
                | Action::ApprovalConsumed { .. }
        );
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
            Action::ContainerGrantRequested { .. }
            | Action::ContainerGrantApplied { .. }
            | Action::ContainerRevokeRequested { .. }
            | Action::ContainerGrantRevoked { .. } => container::apply(state, &entry.action)?,
            Action::ProfileAdded { .. } | Action::ProfileRemoved { .. } => {
                profile::apply(state, &entry.action, revision)?;
            }
            Action::PackageProfileAdded { .. }
            | Action::PackageProfileRemoved { .. }
            | Action::PackageFetchStarted { .. }
            | Action::PackageFetchFinished { .. } => {
                package::apply(state, &entry.action, revision)?;
            }
            Action::PackageCacheRootSet {
                workspace_id,
                directory,
            } => {
                if !directory.validate_shape()
                    || !state
                        .workspaces
                        .iter()
                        .any(|workspace| workspace.id == *workspace_id)
                {
                    return Err(ControlError::Invalid("invalid package cache root"));
                }
                if let Some(existing) = state
                    .package_cache_roots
                    .iter_mut()
                    .find(|status| status.workspace_id == *workspace_id)
                {
                    existing.directory = directory.clone();
                    existing.revision = revision;
                } else {
                    state.package_cache_roots.push(PackageCacheRootStatus {
                        workspace_id: *workspace_id,
                        directory: directory.clone(),
                        revision,
                    });
                }
            }
            Action::PackageCacheRootCleared { workspace_id } => {
                let index = state
                    .package_cache_roots
                    .iter()
                    .position(|status| status.workspace_id == *workspace_id)
                    .ok_or(ControlError::Invalid(
                        "clearing an unknown package cache root",
                    ))?;
                state.package_cache_roots.remove(index);
            }
            Action::ProviderProfileAdded { .. } | Action::ProviderProfileRemoved { .. } => {
                provider::apply(state, &entry.action, revision)?;
            }
            Action::ApprovalRequested { .. }
            | Action::ApprovalDecided { .. }
            | Action::ApprovalConsumed { .. } => {
                approval::apply(state, &entry.action, revision)?;
            }
        }
        state.revision = revision;
        Ok(())
    }
}

/// Restrict store, event, and session metadata to non-nil version 4 identifiers.
fn valid_uuid(value: Uuid) -> bool {
    !value.is_nil() && value.get_version() == Some(Version::Random)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Read through the owner because Windows rejects reads from a second handle.
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

    /// Exhausted settings cannot leave operational authority that cannot be revoked.
    #[test]
    fn operational_checks_refuse_when_settings_have_no_permission_change_capacity() {
        let directory = tempfile::tempdir().expect("private temporary directory");
        let data = directory.path().join("data");
        let root = directory.path().join("projects");
        fs::create_dir(&root).expect("development root");
        let mut store = ControlStore::open(&data).expect("control store");
        store.set_development_root(&root, 0).expect("root");
        let workspace = store.register_workspace(&root, 1).expect("workspace");
        let session_id = Uuid::new_v4();
        let binding = store
            .bind_session(session_id, workspace.id())
            .expect("binding");
        let request = PolicyRequest {
            session_id,
            workspace_id: workspace.id(),
            operation: Operation::Read,
        };
        assert!(
            store
                .check_file_policy(&binding, &request)
                .expect("healthy policy")
                .configured_allowed()
        );
        let bytes = store.bytes;
        store.bytes = MAX_BYTES - 1;
        assert!(matches!(
            store.check_file_policy(&binding, &request),
            Err(ControlError::LimitReached)
        ));
        store.bytes = bytes;
        let initial = store.entries[0].clone();
        store.entries.resize(MAX_EVENTS, initial);
        assert!(matches!(
            store.check_file_policy(&binding, &request),
            Err(ControlError::LimitReached)
        ));
    }

    /// A failed write must leave acknowledged state unchanged and disable the writer.
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

    /// Bytes can reach disk before sync fails, but no unaudited result may escape.
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

    /// Replay must reject tampered envelopes without repairing the original bytes.
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

    /// Exhausted capacities reject mutations rather than discard settings or audit.
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
