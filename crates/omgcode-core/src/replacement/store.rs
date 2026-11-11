//! Private checkpoint storage. All workspace authority stays at the file broker.
//!
//! The caller holds the control writer for every synchronous operation. Nothing
//! here replaces a workspace entry or replays a journal. Only an explicit
//! deletion request removes one resolved entry; no error path deletes evidence.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use omgcode_platform::broker::FileCapture;
use omgcode_platform::private_store::{PrivateDirectory, file_identity, stage_metadata};
use serde::Serialize;
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::manifest::{MAX_MANIFEST, Manifest, SecurityMetadata, descriptor};
use super::replay::{ArtifactNames, Envelope, InitializationAction, LedgerAction};
use super::{
    CheckpointRetention, InitializationPhase, RecordedArtifact, RecordedReplacement,
    ReplacementProgress, ReplacementScope, ReplacementSnapshot, inspect_initialization,
    inspect_ledger,
};
use crate::control::{ControlError, ControlStore};
use crate::files::FileTarget;
use crate::session::NativePath;
use crate::workspace::DirectoryRecord;

mod publication;
pub use publication::ReplacementReceipt;

const ANCHOR: &str = "replacement-init-v1.jsonl";
const DIRECTORY: &str = "replacement-v1";
const LEDGER: &str = "ledger.jsonl";
const MAX_LEDGER: usize = 8 * 1024 * 1024;
const MAX_RECORD: usize = 64 * 1024;
const MAX_EVENTS: usize = 4096;
const MAX_PAYLOAD: usize = 1024 * 1024;
const MAX_STORAGE: u64 = 64 * 1024 * 1024;
// Each remaining v1 stage, commit, finish, or acknowledgement record fits 1 KiB.
const TAIL_RECORD_BYTES: usize = 1024;
const RECOVERY_BYTES: usize = 4 * TAIL_RECORD_BYTES;
const FUTURE_STAGE_BYTES: u64 = 1024 * 1024;
const MAX_RETAINED: usize = 64;
/// Fixed entry artifact names, in deletion order: unpublished and private
/// bytes first, then the manifest that describes them.
const ENTRY_ARTIFACTS: [&str; 4] = ["stage", "proposed", "original", "manifest.json"];

/// A controlled checkpoint error. No error authorizes cleanup or retry.
#[derive(Debug, Error)]
pub enum CheckpointError {
    /// Current settings or their native directory registration failed.
    #[error("checkpoint control failed: {0}")]
    Control(#[from] ControlError),
    /// Native storage or synchronization failed. Preserve its original cause.
    #[error("checkpoint storage I/O failed: {0}")]
    Io(#[from] io::Error),
    /// Stored records failed strict validation.
    #[error("checkpoint records are invalid: {0}")]
    Records(#[from] super::ReplacementError),
    /// JSON syntax or typed shape is invalid. Display omits untrusted text.
    #[error("checkpoint JSON is invalid")]
    Json(#[source] serde_json::Error),
    /// Required objects, layout, or metadata are invalid; nothing is repaired.
    #[error("invalid checkpoint storage: {0}")]
    Invalid(&'static str),
    /// The target differs from the expected content, identity, or metadata.
    #[error("checkpoint target conflicts with expected content, identity, or metadata")]
    Conflict,
    /// No different content was requested; no checkpoint needs to be created.
    #[error("checkpoint content is unchanged")]
    Unchanged,
    /// A live binding or current permission check refused private preparation.
    #[error("checkpoint preparation denied: {0:?}")]
    Denied(crate::policy::DecisionReason),
    /// A fixed quota or journal bound prevents admission.
    #[error("checkpoint storage capacity reached")]
    Limit,
    /// Another owner retains the private journal lock.
    #[error("checkpoint storage is already in use")]
    Busy,
    /// An unresolved operation requires trusted inspection and acknowledgement.
    #[error("checkpoint {0} is unresolved; inspect it before acknowledgement")]
    Pending(Uuid),
    /// This ID does not identify the current unresolved operation.
    #[error("checkpoint ID does not identify the unresolved operation")]
    WrongOperation,
    /// A recorded deletion is unfinished. Only repeating that same explicit
    /// deletion can finish it; other checkpoint work is refused meanwhile.
    #[error("checkpoint {0} deletion is unfinished; repeat that deletion to finish it")]
    DeletionPending(Uuid),
    /// The ID names no retained entry that this request can use.
    #[error("checkpoint ID does not identify a retained checkpoint for this request")]
    Unavailable,
    /// The checkpoint was recorded for a different workspace or target path.
    #[error("checkpoint was recorded for a different workspace or target")]
    TargetMismatch,
    /// A failed write or synchronization prevents further writes on this handle.
    #[error("checkpoint writer is poisoned")]
    Poisoned,
    /// Private evidence may exist. Reopen for inspection; do not retry or delete.
    #[error("checkpoint operation is uncertain for operation {operation_id:?}: {source}")]
    Uncertain {
        /// None denotes initialization, before an operation was admitted.
        operation_id: Option<Uuid>,
        /// The original failure, retained without automatic recovery.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl CheckpointError {
    /// Stable operator-facing category. This does not change the stored state.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Control(_) => "control",
            Self::Io(error) if error.kind() == io::ErrorKind::Unsupported => {
                "checkpoint_unsupported"
            }
            Self::Io(_) => "checkpoint_io",
            Self::Records(_) | Self::Json(_) | Self::Invalid(_) => "checkpoint_invalid",
            Self::Conflict => "checkpoint_conflict",
            Self::Unchanged => "checkpoint_unchanged",
            Self::Denied(_) => "checkpoint_denied",
            Self::Limit => "checkpoint_limit",
            Self::Busy => "checkpoint_busy",
            Self::Pending(_) => "checkpoint_pending",
            Self::WrongOperation => "checkpoint_request",
            Self::DeletionPending(_) => "checkpoint_deletion_pending",
            Self::Unavailable => "checkpoint_unavailable",
            Self::TargetMismatch => "checkpoint_target",
            Self::Poisoned => "checkpoint_poisoned",
            Self::Uncertain { .. } => "checkpoint_uncertain",
        }
    }
}

/// A prepared private checkpoint. It is neither a replacement nor a write grant.
#[derive(Debug, Serialize)]
pub struct CheckpointReceipt {
    /// Retained operation UUID and private-entry locator.
    pub operation_id: Uuid,
    /// Exact original-byte descriptor, not the file contents.
    pub original: RecordedArtifact,
    /// Exact proposed-byte descriptor, not the file contents.
    pub proposed: RecordedArtifact,
    /// Always false for preparation without publication.
    pub applied: bool,
}

/// Outcome of one explicit deletion. No workspace file is opened or changed.
#[derive(Debug, Serialize)]
pub struct CheckpointDeletion {
    /// Deleted operation ID. Its ledger history and outcome are retained.
    pub operation_id: Uuid,
    /// Whether this request finished a deletion that an earlier one recorded.
    pub resumed: bool,
    /// Observed managed file bytes after the deletion.
    pub managed_bytes: u64,
}

/// Observed protected-storage phase. No variant grants authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointState {
    /// Neither checkpoint path exists; complete erasure is indistinguishable.
    Unused,
    /// The anchor records initialization without a complete ready transition.
    Initializing,
    /// The ready anchor, ledger, and required artifacts passed validation.
    Ready,
}

/// Existing-only metadata inspection. Payload bytes never appear in this view.
#[derive(Debug, Serialize)]
pub struct CheckpointInspection {
    /// Observed storage phase; no variant grants authority.
    pub state: CheckpointState,
    /// Current control-store identity.
    pub control_store_id: Uuid,
    /// Replacement-store identity, when an anchor exists.
    pub replacement_store_id: Option<Uuid>,
    /// One unfinished or uncertain operation, if any.
    pub pending: Option<Uuid>,
    /// One recorded, unfinished deletion, if any.
    pub pending_deletion: Option<Uuid>,
    /// All recorded operation metadata, including deleted entries' history.
    /// At most 64 operations are not deleted.
    pub operations: Vec<RecordedReplacement>,
    /// Observed managed file bytes including anchor, ledger, and partial files.
    pub managed_bytes: u64,
}

/// Observe absence only. Creation is serialized by the caller's control lock.
/// Legacy unused data directories do not acquire stricter mode requirements.
fn state_present(path: &Path) -> Result<bool, CheckpointError> {
    let mut present = false;
    for name in [ANCHOR, DIRECTORY] {
        match fs::symlink_metadata(path.join(name)) {
            Ok(_) => present = true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(present)
}

fn context(control: &ControlStore) -> Result<(DirectoryRecord, Uuid), CheckpointError> {
    Ok(control.replacement_context()?)
}

/// Bind a private native root to the registered object, including creation time.
/// A matching path alone cannot substitute another application-data directory.
fn open_root(expected: &DirectoryRecord) -> Result<PrivateDirectory, CheckpointError> {
    let root = PrivateDirectory::open(&expected.live_path()?)?;
    if root.identity()? != expected.native_identity() || !expected.matches_created(root.created()?)
    {
        return Err(CheckpointError::Invalid(
            "opened data directory differs from registration",
        ));
    }
    Ok(root)
}

/// Validate existing evidence before the caller opens the legacy operation log.
/// Incomplete initialization and pending work block admission, not settings.
pub(crate) fn check_admission(control: &ControlStore) -> Result<(), CheckpointError> {
    let view = inspect(control)?;
    if view.state == CheckpointState::Initializing {
        return Err(CheckpointError::Invalid(
            "initialization is incomplete; inspect existing storage",
        ));
    }
    if let Some(id) = view.pending {
        return Err(CheckpointError::Pending(id));
    }
    if let Some(id) = view.pending_deletion {
        return Err(CheckpointError::DeletionPending(id));
    }
    Ok(())
}

/// Inspect under the caller's control lock and a retained exclusive anchor lock.
/// Absence never provisions storage; initializing state permits only a bounded
/// partial inventory, while ready state requires full artifact validation.
pub(crate) fn inspect(control: &ControlStore) -> Result<CheckpointInspection, CheckpointError> {
    let (expected, control_id) = context(control)?;
    if !state_present(&expected.live_path()?)? {
        return Ok(CheckpointInspection {
            state: CheckpointState::Unused,
            control_store_id: control_id,
            replacement_store_id: None,
            pending: None,
            pending_deletion: None,
            operations: Vec::new(),
            managed_bytes: 0,
        });
    }
    let root = open_root(&expected)?;
    let mut anchor = root.open_file(OsStr::new(ANCHOR), false)?;
    lock(&anchor)?;
    let anchor_bytes = read_bounded(&mut anchor, 4096)?;
    let initialization = inspect_initialization(&anchor_bytes, control_id)?;
    if initialization.phase == InitializationPhase::Initializing {
        // No operation can precede ready. Preserve even an empty/partial ledger
        // as initialization evidence without claiming that it can be replayed.
        let mut used = u64::try_from(anchor_bytes.len()).map_err(|_| CheckpointError::Limit)?;
        if root.exists(OsStr::new(DIRECTORY))? {
            let directory = root.open_dir(OsStr::new(DIRECTORY))?;
            for name in directory.names(1)? {
                if name != OsStr::new(LEDGER) {
                    return Err(CheckpointError::Invalid(
                        "unexpected initialization artifact",
                    ));
                }
                let file = directory.open_file(&name, false)?;
                add_usage(&mut used, file.metadata()?.len())?;
                if file.metadata()?.len()
                    > u64::try_from(MAX_LEDGER).map_err(|_| CheckpointError::Limit)?
                {
                    return Err(CheckpointError::Limit);
                }
            }
        }
        return Ok(CheckpointInspection {
            state: CheckpointState::Initializing,
            control_store_id: control_id,
            replacement_store_id: Some(initialization.scope.replacement_store_id()),
            pending: None,
            pending_deletion: None,
            operations: Vec::new(),
            managed_bytes: used,
        });
    }
    let store = Store::from_opened(root, anchor, anchor_bytes, initialization.scope, false)?;
    Ok(store.view())
}

/// Open existing private storage writable only to append an acknowledgement.
/// Invalid evidence is not repaired and workspace authority is not reconstructed.
pub(crate) fn acknowledge(
    control: &ControlStore,
    operation_id: Uuid,
) -> Result<(), CheckpointError> {
    let (expected, control_id) = context(control)?;
    if !state_present(&expected.live_path()?)? {
        return Err(CheckpointError::WrongOperation);
    }
    let mut store = Store::open_ready(open_root(&expected)?, control_id, true)?;
    record_acknowledgement(&mut store, operation_id)
}

/// Delete one resolved entry, or finish the recorded deletion of that entry.
/// The ledger keeps the operation's history. No workspace file is opened.
pub(crate) fn delete(
    control: &ControlStore,
    operation_id: Uuid,
) -> Result<CheckpointDeletion, CheckpointError> {
    delete_with(control, operation_id, Faults::default())
}

/// Delete with one instance-local test fault at a named persistence boundary.
#[cfg(test)]
pub(crate) fn delete_with_fault(
    control: &ControlStore,
    operation_id: Uuid,
    point: &'static str,
) -> Result<CheckpointDeletion, CheckpointError> {
    delete_with(control, operation_id, Faults { at: Some(point) })
}

fn delete_with(
    control: &ControlStore,
    operation_id: Uuid,
    faults: Faults,
) -> Result<CheckpointDeletion, CheckpointError> {
    let (expected, control_id) = context(control)?;
    if !state_present(&expected.live_path()?)? {
        return Err(CheckpointError::Unavailable);
    }
    let mut store = Store::open_ready(open_root(&expected)?, control_id, true)?;
    store.faults = faults;
    store.delete_entry(operation_id)
}

/// Return one retained checkpoint's original bytes for an explicit restore.
///
/// The entry must be prepared and recorded for the same workspace and native
/// target path. Its bytes are reread and compared with the recorded digest.
/// The store is opened read-only and closed before the caller applies them.
pub(crate) fn read_original(
    control: &ControlStore,
    operation_id: Uuid,
    target: &FileTarget,
) -> Result<Vec<u8>, CheckpointError> {
    let (expected, control_id) = context(control)?;
    if !state_present(&expected.live_path()?)? {
        return Err(CheckpointError::Unavailable);
    }
    let store = Store::open_ready(open_root(&expected)?, control_id, false)?;
    if let Some(operation) = store.snapshot.pending() {
        return Err(CheckpointError::Pending(operation.operation_id));
    }
    if let Some(operation) = store.snapshot.pending_deletion() {
        return Err(CheckpointError::DeletionPending(operation.operation_id));
    }
    let operation = store
        .snapshot
        .operations
        .iter()
        .find(|operation| {
            operation.operation_id == operation_id
                && operation.retention == CheckpointRetention::Retained
                && operation.manifest.is_some()
        })
        .ok_or(CheckpointError::Unavailable)?;
    let path = NativePath::from_path(&target.path)
        .map_err(|_| CheckpointError::Invalid("native target"))?;
    if operation.workspace_id != target.workspace_id || operation.target != path {
        return Err(CheckpointError::TargetMismatch);
    }
    let entry = store
        .directory
        .open_dir(OsStr::new(&operation_id.to_string()))?;
    let mut file = entry.open_file(OsStr::new("original"), false)?;
    let bytes = read_bounded(&mut file, MAX_PAYLOAD)?;
    if descriptor(&bytes)? != operation.expected || std::str::from_utf8(&bytes).is_err() {
        return Err(CheckpointError::Invalid(
            "checkpoint payload differs or is not UTF-8",
        ));
    }
    Ok(bytes)
}

/// Clear only the current blocker, preserving progress and any recorded outcome.
/// A poisoned append is uncertain even if a complete record becomes visible.
fn record_acknowledgement(store: &mut Store, operation_id: Uuid) -> Result<(), CheckpointError> {
    if store
        .snapshot
        .pending()
        .map(|operation| operation.operation_id)
        != Some(operation_id)
    {
        return Err(CheckpointError::WrongOperation);
    }
    if let Err(source) = store.append(
        LedgerAction::ReplacementAcknowledged { operation_id },
        0,
        0,
        "ack_sync",
    ) {
        if store.poisoned {
            return Err(CheckpointError::Uncertain {
                operation_id: Some(operation_id),
                source: Box::new(source),
            });
        }
        return Err(source);
    }
    Ok(())
}

pub(crate) struct Store {
    root: PrivateDirectory,
    directory: PrivateDirectory,
    ledger: File,
    bytes: Vec<u8>,
    snapshot: ReplacementSnapshot,
    used: u64,
    poisoned: bool,
    faults: Faults,
    // Fields drop in declaration order. Keep this lock until dependent files
    // and directories have closed, including during unwinding.
    _anchor: File,
}

/// Instance-local test faults never change process state or relax a sync gate.
#[derive(Default)]
struct Faults {
    #[cfg(test)]
    at: Option<&'static str>,
}

impl Faults {
    /// Fail once at a named instance-local persistence boundary.
    fn check(&mut self, _point: &str) -> io::Result<()> {
        #[cfg(test)]
        if self.at == Some(_point) {
            self.at = None;
            return Err(io::Error::other("injected checkpoint persistence failure"));
        }
        Ok(())
    }

    /// Inject a partial record only in tests; production writes the exact bytes.
    /// Preserve the written prefix when reporting failure so reopen sees reality.
    fn write(&mut self, file: &mut File, bytes: &[u8], _point: &str) -> io::Result<()> {
        #[cfg(test)]
        {
            let partial = match _point {
                "initializing_sync" => "initializing_partial_write",
                "header_sync" => "header_partial_write",
                "ready_sync" => "ready_partial_write",
                "intent_sync" => "intent_partial_write",
                "prepared_sync" => "prepared_partial_write",
                "stage_prepared_sync" => "stage_prepared_partial_write",
                "commit_intent_sync" => "commit_intent_partial_write",
                "publication_outcome_sync" => "publication_outcome_partial_write",
                "ack_sync" => "ack_partial_write",
                "deletion_intent_sync" => "deletion_intent_partial_write",
                "deletion_outcome_sync" => "deletion_outcome_partial_write",
                _ => "unselected_partial_write",
            };
            if self.at == Some(partial) {
                self.at = None;
                let prefix = bytes
                    .get(..bytes.len() / 2)
                    .ok_or_else(|| io::Error::other("invalid fault prefix"))?;
                file.write_all(prefix)?;
                return Err(io::Error::other("injected partial checkpoint write"));
            }
        }
        file.write_all(bytes)
    }
}

impl Store {
    /// Select existing storage or initialize once under the caller's control lock.
    /// Check the source device and sync support before creation, then require
    /// the caller's final authority check. Initialization errors preserve evidence.
    pub(crate) fn open_or_initialize(
        control: &ControlStore,
        volume: u64,
        before_mutation: impl FnOnce(&PrivateDirectory) -> Result<(), CheckpointError>,
    ) -> Result<Self, CheckpointError> {
        let (expected, control_id) = context(control)?;
        let root = open_root(&expected)?;
        if root.identity()?.volume != volume {
            return Err(CheckpointError::Invalid(
                "checkpoint and source must share a filesystem",
            ));
        }
        if state_present(&expected.live_path()?)? {
            let store = Self::open_ready(root, control_id, true)?;
            before_mutation(&store.root)?;
            return Ok(store);
        }
        // Probe required parent synchronization before creating the first
        // anchor. This is not a power-loss qualification of the filesystem.
        root.sync()?;
        before_mutation(&root)?;
        Self::initialize(root, control_id, Faults::default()).map_err(|source| {
            CheckpointError::Uncertain {
                operation_id: None,
                source: Box::new(source),
            }
        })
    }

    /// Persist initializing, ledger header, then ready through exclusive names.
    /// Each transition syncs its files and parents; no error path removes them.
    fn initialize(
        root: PrivateDirectory,
        control_id: Uuid,
        mut faults: Faults,
    ) -> Result<Self, CheckpointError> {
        // Recheck through the retained root, never replacing either candidate.
        if root.exists(OsStr::new(ANCHOR))? || root.exists(OsStr::new(DIRECTORY))? {
            return Err(CheckpointError::Invalid(
                "replacement storage already exists",
            ));
        }
        let scope = ReplacementScope::new(control_id, Uuid::new_v4())?;
        let initial = encode(scope, 0, InitializationAction::Initializing)?;
        let ready = encode(scope, 1, InitializationAction::Ready)?;
        if initial
            .len()
            .checked_add(ready.len())
            .is_none_or(|size| size > 4096)
        {
            return Err(CheckpointError::Limit);
        }
        let mut anchor = root.create_file(OsStr::new(ANCHOR))?;
        lock(&anchor)?;
        faults.write(&mut anchor, &initial, "initializing_sync")?;
        faults.check("initializing_sync")?;
        anchor.sync_all()?;
        root.sync()?;
        let directory = root.create_dir(OsStr::new(DIRECTORY))?;
        faults.check("store_creation")?;
        let mut ledger = directory.create_file(OsStr::new(LEDGER))?;
        let bytes = encode(scope, 0, LedgerAction::Created)?;
        faults.write(&mut ledger, &bytes, "header_sync")?;
        faults.check("header_sync")?;
        ledger.sync_all()?;
        directory.sync()?;
        root.sync()?;
        faults.write(&mut anchor, &ready, "ready_sync")?;
        faults.check("ready_sync")?;
        anchor.sync_all()?;
        root.sync()?;
        let mut anchor_bytes = initial;
        anchor_bytes.extend_from_slice(&ready);
        let snapshot = inspect_ledger(&bytes, scope)?;
        let used = inventory(&root, &directory, &snapshot, anchor_bytes.len())?;
        Ok(Self {
            root,
            _anchor: anchor,
            directory,
            ledger,
            bytes,
            snapshot,
            used,
            poisoned: false,
            faults: Faults::default(),
        })
    }

    /// Acquire the anchor lock before opening dependent handles. Incomplete or
    /// missing initialization is never interpreted as permission to start over.
    fn open_ready(
        root: PrivateDirectory,
        control_id: Uuid,
        writable: bool,
    ) -> Result<Self, CheckpointError> {
        let mut anchor = root.open_file(OsStr::new(ANCHOR), writable)?;
        lock(&anchor)?;
        let anchor_bytes = read_bounded(&mut anchor, 4096)?;
        let initialization = inspect_initialization(&anchor_bytes, control_id)?;
        if initialization.phase != InitializationPhase::Ready {
            return Err(CheckpointError::Invalid(
                "checkpoint initialization is incomplete",
            ));
        }
        Self::from_opened(root, anchor, anchor_bytes, initialization.scope, writable)
    }

    /// Retain the locked anchor while decoding the ledger and validating files.
    /// Existing-only opens prevent a missing referenced ledger becoming empty.
    fn from_opened(
        root: PrivateDirectory,
        anchor: File,
        anchor_bytes: Vec<u8>,
        scope: ReplacementScope,
        writable: bool,
    ) -> Result<Self, CheckpointError> {
        let directory = root.open_dir(OsStr::new(DIRECTORY))?;
        let mut ledger = directory.open_file(OsStr::new(LEDGER), writable)?;
        let bytes = read_bounded(&mut ledger, MAX_LEDGER)?;
        let snapshot = inspect_ledger(&bytes, scope)?;
        let used = inventory(&root, &directory, &snapshot, anchor_bytes.len())?;
        Ok(Self {
            root,
            _anchor: anchor,
            directory,
            ledger,
            bytes,
            snapshot,
            used,
            poisoned: false,
            faults: Faults::default(),
        })
    }

    fn view(&self) -> CheckpointInspection {
        CheckpointInspection {
            state: CheckpointState::Ready,
            control_store_id: self.snapshot.scope.control_store_id(),
            replacement_store_id: Some(self.snapshot.scope.replacement_store_id()),
            pending: self
                .snapshot
                .pending()
                .map(|operation| operation.operation_id),
            pending_deletion: self
                .snapshot
                .pending_deletion()
                .map(|operation| operation.operation_id),
            operations: self.snapshot.operations.clone(),
            managed_bytes: self.used,
        }
    }

    /// Reserve the full future storage tail before recording intent and artifacts.
    /// Stop unresolved after verified preparation; any error after intent starts
    /// poisons this writer and reports uncertainty without workspace publication.
    pub(crate) fn prepare(
        &mut self,
        target: &FileTarget,
        revision: u64,
        capture: &FileCapture,
        proposed: &[u8],
    ) -> Result<CheckpointReceipt, CheckpointError> {
        if self.poisoned {
            return Err(CheckpointError::Poisoned);
        }
        if let Some(operation) = self.snapshot.pending() {
            return Err(CheckpointError::Pending(operation.operation_id));
        }
        if let Some(operation) = self.snapshot.pending_deletion() {
            return Err(CheckpointError::DeletionPending(operation.operation_id));
        }
        if self
            .snapshot
            .operations
            .iter()
            .filter(|operation| operation.retention != CheckpointRetention::Deleted)
            .count()
            >= MAX_RETAINED
        {
            return Err(CheckpointError::Limit);
        }
        let operation_id = Uuid::new_v4();
        let original = descriptor(&capture.bytes)?;
        let proposed_descriptor = descriptor(proposed)?;
        let manifest = Manifest::encode(
            self.snapshot.scope,
            operation_id,
            target,
            revision,
            capture,
            proposed,
        )?;
        let start = LedgerAction::ReplacementStarted {
            operation_id,
            session_id: target.session_id,
            workspace_id: *target.workspace_id.as_uuid(),
            revision,
            target: crate::session::NativePath::from_path(&target.path)
                .map_err(|_| CheckpointError::Invalid("native target"))?,
            expected: original.clone(),
            proposed: proposed_descriptor.clone(),
            artifacts: ArtifactNames {
                original: "original".to_owned(),
                proposed: "proposed".to_owned(),
                manifest: "manifest.json".to_owned(),
                stage: "stage".to_owned(),
            },
        };
        let intent = encode(self.snapshot.scope, self.snapshot.event_count, &start)?;
        // Reserve maximum originals/proposal/stage, manifest, and all journal tail
        // capacity before making any artifact. No eviction makes capacity appear.
        let reserve = 3u64
            .checked_mul(FUTURE_STAGE_BYTES)
            .and_then(|size| {
                size.checked_add(u64::try_from(MAX_MANIFEST + RECOVERY_BYTES + 1024).ok()?)
            })
            .and_then(|size| size.checked_add(u64::try_from(intent.len()).ok()?))
            .ok_or(CheckpointError::Limit)?;
        ensure_budget(self.used, reserve)?;
        if self
            .snapshot
            .event_count
            .checked_add(6)
            .is_none_or(|size| size > MAX_EVENTS)
            || self
                .bytes
                .len()
                .checked_add(intent.len())
                .and_then(|size| size.checked_add(RECOVERY_BYTES + 1024))
                .is_none_or(|size| size > MAX_LEDGER)
        {
            return Err(CheckpointError::Limit);
        }
        let result = (|| {
            self.append(start, 5, RECOVERY_BYTES, "intent_sync")?;
            let entry = self
                .directory
                .create_dir(OsStr::new(&operation_id.to_string()))?;
            self.faults.check("entry_creation")?;
            self.write_payload(&entry, "original", &capture.bytes)?;
            self.write_payload(&entry, "proposed", proposed)?;
            self.write_payload(&entry, "manifest.json", &manifest)?;
            self.faults.check("checkpoint_parent_sync")?;
            entry.sync()?;
            self.directory.sync()?;
            self.root.sync()?;
            // Reopen the actual payloads and strict manifest before publishing a
            // prepared record. This does not reopen the workspace target.
            let operation = self
                .snapshot
                .operations
                .last()
                .ok_or(CheckpointError::Invalid("missing intent"))?;
            let manifest_reference = descriptor(&manifest)?;
            validate_prepared(
                &entry,
                operation,
                self.snapshot.scope,
                capture.volume,
                &manifest_reference,
            )?;
            self.append(
                LedgerAction::CheckpointPrepared {
                    operation_id,
                    manifest: manifest_reference,
                },
                4,
                RECOVERY_BYTES,
                "prepared_sync",
            )?;
            self.refresh_usage()?;
            Ok(CheckpointReceipt {
                operation_id,
                original,
                proposed: proposed_descriptor,
                applied: false,
            })
        })();
        result.map_err(|source: CheckpointError| {
            self.poisoned = true;
            CheckpointError::Uncertain {
                operation_id: Some(operation_id),
                source: Box::new(source),
            }
        })
    }

    /// Record deletion intent, remove the entry's fixed artifacts and then the
    /// empty entry, and record completion. A repeated request for a recorded
    /// deletion skips the intent and removes whatever fixed names remain.
    ///
    /// The intent reserves its completion record. Errors before the intent is
    /// written change nothing. Any later failure poisons this writer and is
    /// uncertain; the deletion stays pending until the same request finishes it.
    fn delete_entry(&mut self, operation_id: Uuid) -> Result<CheckpointDeletion, CheckpointError> {
        if self.poisoned {
            return Err(CheckpointError::Poisoned);
        }
        let resumed = match self.snapshot.pending_deletion() {
            Some(operation) if operation.operation_id == operation_id => true,
            Some(operation) => {
                return Err(CheckpointError::DeletionPending(operation.operation_id));
            }
            None => {
                if let Some(operation) = self.snapshot.pending() {
                    return Err(CheckpointError::Pending(operation.operation_id));
                }
                if !self.snapshot.operations.iter().any(|operation| {
                    operation.operation_id == operation_id
                        && operation.retention == CheckpointRetention::Retained
                }) {
                    return Err(CheckpointError::Unavailable);
                }
                false
            }
        };
        if !resumed
            && let Err(source) = self.append(
                LedgerAction::CheckpointDeletionStarted { operation_id },
                1,
                TAIL_RECORD_BYTES,
                "deletion_intent_sync",
            )
        {
            if self.poisoned {
                return Err(CheckpointError::Uncertain {
                    operation_id: Some(operation_id),
                    source: Box::new(source),
                });
            }
            return Err(source);
        }
        let result = (|| {
            self.remove_entry(operation_id)?;
            self.append(
                LedgerAction::CheckpointDeleted { operation_id },
                0,
                0,
                "deletion_outcome_sync",
            )?;
            self.refresh_usage()?;
            Ok(CheckpointDeletion {
                operation_id,
                resumed,
                managed_bytes: self.used,
            })
        })();
        result.map_err(|source: CheckpointError| {
            self.poisoned = true;
            CheckpointError::Uncertain {
                operation_id: Some(operation_id),
                source: Box::new(source),
            }
        })
    }

    /// Remove only the fixed artifact names, then the empty entry directory.
    /// Removal is never recursive: an unknown name makes the directory removal
    /// fail and leaves the deletion pending. Each parent is flushed afterwards;
    /// on Windows that flush carries no power-loss guarantee (ADR-0023).
    fn remove_entry(&mut self, operation_id: Uuid) -> Result<(), CheckpointError> {
        let name = operation_id.to_string();
        if self.directory.exists(OsStr::new(&name))? {
            {
                // Windows directory handles deny delete sharing, so this
                // handle closes before its directory is removed.
                let entry = self.directory.open_dir(OsStr::new(&name))?;
                for artifact in ENTRY_ARTIFACTS {
                    if entry.exists(OsStr::new(artifact))? {
                        entry.remove_file(OsStr::new(artifact))?;
                        self.faults.check("deletion_artifact")?;
                    }
                }
                entry.sync()?;
            }
            self.directory.remove_dir(OsStr::new(&name))?;
            self.faults.check("deletion_directory")?;
        }
        // An interrupted earlier request may have removed the entry without
        // flushing its parent; flush on every path before recording success.
        self.directory.sync()?;
        Ok(())
    }

    fn refresh_usage(&mut self) -> Result<(), CheckpointError> {
        let anchor_bytes =
            usize::try_from(self._anchor.metadata()?.len()).map_err(|_| CheckpointError::Limit)?;
        self.used = inventory(&self.root, &self.directory, &self.snapshot, anchor_bytes)?;
        Ok(())
    }

    /// Create one exclusive artifact, sync its bytes and parent, then count it.
    /// A failure may leave partial data and must not trigger deletion or retry.
    fn write_payload(
        &mut self,
        entry: &PrivateDirectory,
        name: &str,
        bytes: &[u8],
    ) -> Result<(), CheckpointError> {
        let mut file = entry.create_file(OsStr::new(name))?;
        file.write_all(bytes)?;
        self.faults.check(name)?;
        file.sync_all()?;
        entry.sync()?;
        self.used = self
            .used
            .checked_add(u64::try_from(bytes.len()).map_err(|_| CheckpointError::Limit)?)
            .ok_or(CheckpointError::Limit)?;
        Ok(())
    }

    /// Replay and bound the candidate history before changing the existing ledger.
    /// Compare retained bytes again, sync the append, then publish memory. Failed
    /// writes or syncs poison the writer; callers must inspect evidence on reopen.
    fn append(
        &mut self,
        action: LedgerAction,
        reserve_events: usize,
        reserve_bytes: usize,
        sync_point: &str,
    ) -> Result<(), CheckpointError> {
        if self.poisoned {
            return Err(CheckpointError::Poisoned);
        }
        let encoded = encode(self.snapshot.scope, self.snapshot.event_count, action)?;
        let size = self
            .bytes
            .len()
            .checked_add(encoded.len())
            .ok_or(CheckpointError::Limit)?;
        if encoded.len() > MAX_RECORD
            || size
                .checked_add(reserve_bytes)
                .is_none_or(|total| total > MAX_LEDGER)
            || self
                .snapshot
                .event_count
                .checked_add(1)
                .and_then(|count| count.checked_add(reserve_events))
                .is_none_or(|count| count > MAX_EVENTS)
        {
            return Err(CheckpointError::Limit);
        }
        let growth = u64::try_from(
            encoded
                .len()
                .checked_add(reserve_bytes)
                .ok_or(CheckpointError::Limit)?,
        )
        .map_err(|_| CheckpointError::Limit)?;
        ensure_budget(self.used, growth)?;
        let mut candidate = Vec::with_capacity(size);
        candidate.extend_from_slice(&self.bytes);
        candidate.extend_from_slice(&encoded);
        let snapshot = inspect_ledger(&candidate, self.snapshot.scope)?;
        let expected_length =
            u64::try_from(self.bytes.len()).map_err(|_| CheckpointError::Limit)?;
        if self.ledger.metadata()?.len() != expected_length {
            return Err(CheckpointError::Invalid("ledger changed while open"));
        }
        if read_bounded(&mut self.ledger, MAX_LEDGER)? != self.bytes {
            return Err(CheckpointError::Invalid("ledger bytes changed while open"));
        }
        let result = (|| {
            self.ledger.seek(SeekFrom::End(0))?;
            self.faults.write(&mut self.ledger, &encoded, sync_point)?;
            // Publication of memory follows acknowledged sync, never write alone.
            self.faults.check(sync_point)?;
            self.ledger.sync_all()
        })();
        if let Err(error) = result {
            self.poisoned = true;
            return Err(error.into());
        }
        self.used = self
            .used
            .checked_add(u64::try_from(encoded.len()).map_err(|_| CheckpointError::Limit)?)
            .ok_or(CheckpointError::Limit)?;
        self.bytes = candidate;
        self.snapshot = snapshot;
        Ok(())
    }
}

/// Encode one bounded LF-terminated v1 record with fresh event identity and UTC.
/// The append path separately validates its action against the preceding history.
fn encode<A: Serialize>(
    scope: ReplacementScope,
    sequence: usize,
    action: A,
) -> Result<Vec<u8>, CheckpointError> {
    let entry = Envelope {
        schema_version: 1,
        control_store_id: scope.control_store_id(),
        replacement_store_id: scope.replacement_store_id(),
        event_id: Uuid::new_v4(),
        sequence: u64::try_from(sequence).map_err(|_| CheckpointError::Limit)?,
        timestamp: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|_| CheckpointError::Invalid("timestamp format"))?,
        action,
    };
    let mut bytes = serde_json::to_vec(&entry).map_err(CheckpointError::Json)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_RECORD {
        return Err(CheckpointError::Limit);
    }
    Ok(bytes)
}

/// Read a private single-link file with a streaming cap and final length check.
/// Checks use this same handle; they do not promise isolation from same-user writes.
fn read_bounded(file: &mut File, limit: usize) -> Result<Vec<u8>, CheckpointError> {
    read_checked(file, limit, |file| file_identity(file).map(|_| ()))
}

fn read_stage(file: &mut File) -> Result<Vec<u8>, CheckpointError> {
    read_checked(file, MAX_PAYLOAD, |file| stage_metadata(file).map(|_| ()))
}

fn read_checked(
    file: &mut File,
    limit: usize,
    validate: impl Fn(&File) -> io::Result<()>,
) -> Result<Vec<u8>, CheckpointError> {
    validate(file)?;
    let maximum = u64::try_from(limit).map_err(|_| CheckpointError::Limit)?;
    if file.metadata()?.len() > maximum {
        return Err(CheckpointError::Limit);
    }
    let mut bytes = Vec::new();
    file.seek(SeekFrom::Start(0))?;
    Read::by_ref(file)
        .take(maximum.checked_add(1).ok_or(CheckpointError::Limit)?)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(CheckpointError::Limit);
    }
    validate(file)?;
    if file.metadata()?.len() != u64::try_from(bytes.len()).map_err(|_| CheckpointError::Limit)? {
        return Err(CheckpointError::Invalid(
            "private file changed while reading",
        ));
    }
    Ok(bytes)
}

/// Count actual managed file lengths within bounded, nonrecursive enumeration.
/// Partial intent artifacts count too. Prepared artifacts and recorded stages
/// require exact validation; unknown names and missing required evidence refuse.
fn inventory(
    root: &PrivateDirectory,
    directory: &PrivateDirectory,
    snapshot: &ReplacementSnapshot,
    anchor_len: usize,
) -> Result<u64, CheckpointError> {
    let volume = root.identity()?.volume;
    let mut used = u64::try_from(anchor_len).map_err(|_| CheckpointError::Limit)?;
    add_usage(
        &mut used,
        u64::try_from(snapshot.encoded_bytes).map_err(|_| CheckpointError::Limit)?,
    )?;
    let names = directory.names(65)?;
    let mut saw_ledger = false;
    let mut seen = Vec::new();
    for name in names {
        if name == OsStr::new(LEDGER) {
            saw_ledger = true;
            continue;
        }
        let id = name
            .to_str()
            .and_then(|name| Uuid::parse_str(name).ok())
            .ok_or(CheckpointError::Invalid("unknown private store entry"))?;
        if name != OsStr::new(&id.to_string()) {
            return Err(CheckpointError::Invalid("noncanonical private entry name"));
        }
        let operation = snapshot
            .operations
            .iter()
            .find(|operation| operation.operation_id == id)
            .ok_or(CheckpointError::Invalid("unrecorded private entry"))?;
        if operation.retention == CheckpointRetention::Deleted {
            return Err(CheckpointError::Invalid(
                "a deleted checkpoint entry is still present",
            ));
        }
        let entry = directory.open_dir(&name)?;
        for filename in entry.names(4)? {
            let limit = if filename == OsStr::new("manifest.json") {
                MAX_MANIFEST
            } else if ["original", "proposed", "stage"]
                .iter()
                .any(|name| filename == OsStr::new(name))
            {
                MAX_PAYLOAD
            } else {
                return Err(CheckpointError::Invalid("unknown checkpoint artifact"));
            };
            let file = if filename == OsStr::new("stage") {
                entry.open_stage()?
            } else {
                entry.open_file(&filename, false)?
            };
            let length = file.metadata()?.len();
            if length > u64::try_from(limit).map_err(|_| CheckpointError::Limit)? {
                return Err(CheckpointError::Limit);
            }
            add_usage(&mut used, length)?;
        }
        if operation.retention == CheckpointRetention::Deleting {
            // A recorded deletion can leave any subset of the fixed names.
            // Count them; their content no longer needs to match its record.
            seen.push(id);
            continue;
        }
        let security = if operation.progress != ReplacementProgress::Intent {
            let reference = operation
                .manifest
                .as_ref()
                .ok_or(CheckpointError::Invalid("missing manifest reference"))?;
            Some(validate_prepared(
                &entry,
                operation,
                snapshot.scope,
                volume,
                reference,
            )?)
        } else {
            None
        };
        if let Some(identity) = &operation.stage_identity {
            if entry.exists(OsStr::new("stage"))? {
                let security = security.ok_or(CheckpointError::Invalid("stage lacks manifest"))?;
                validate_stage(
                    &entry,
                    identity,
                    &operation.proposed,
                    security,
                    operation.progress == ReplacementProgress::CommitIntended,
                )?;
            } else if operation.progress != ReplacementProgress::CommitIntended {
                return Err(CheckpointError::Invalid("recorded stage is missing"));
            }
        }
        seen.push(id);
    }
    if !saw_ledger {
        return Err(CheckpointError::Invalid(
            "checkpoint ledger entry is missing",
        ));
    }
    if snapshot.operations.iter().any(|operation| {
        operation.retention == CheckpointRetention::Retained
            && operation.progress != ReplacementProgress::Intent
            && !seen.contains(&operation.operation_id)
    }) {
        return Err(CheckpointError::Invalid(
            "prepared checkpoint directory is missing",
        ));
    }
    Ok(used)
}

/// Reopen the exact manifest and payloads through the retained operation handle.
/// Validate strict scope, sizes, digests, metadata shape, and UTF-8 without opening
/// the workspace target or inferring that a recorded native commit occurred.
fn validate_prepared(
    entry: &PrivateDirectory,
    operation: &RecordedReplacement,
    scope: ReplacementScope,
    volume: u64,
    reference: &RecordedArtifact,
) -> Result<SecurityMetadata, CheckpointError> {
    let mut manifest = entry.open_file(OsStr::new("manifest.json"), false)?;
    let manifest_bytes = read_bounded(&mut manifest, MAX_MANIFEST)?;
    if descriptor(&manifest_bytes)? != *reference {
        return Err(CheckpointError::Invalid("manifest digest or size differs"));
    }
    let security = Manifest::validate(&manifest_bytes, scope, operation, volume)?;
    for (name, expected) in [
        ("original", &operation.expected),
        ("proposed", &operation.proposed),
    ] {
        let mut file = entry.open_file(OsStr::new(name), false)?;
        let bytes = read_bounded(&mut file, MAX_PAYLOAD)?;
        if descriptor(&bytes)? != *expected || std::str::from_utf8(&bytes).is_err() {
            return Err(CheckpointError::Invalid(
                "checkpoint payload differs or is not UTF-8",
            ));
        }
    }
    Ok(security)
}

/// Validate the exact retained stage against its identity and the security its
/// progress permits. See `SecurityMetadata::permits_stage`.
pub(super) fn validate_stage(
    entry: &PrivateDirectory,
    identity: &super::RecordedFileIdentity,
    proposed: &RecordedArtifact,
    security: SecurityMetadata,
    after_commit_intent: bool,
) -> Result<u64, CheckpointError> {
    let mut file = entry.open_stage()?;
    let bytes = read_stage(&mut file)?;
    let actual = stage_metadata(&file)?;
    let private = stage_is_private(&file)?;
    let content = descriptor(&bytes)?;
    if actual.identity.volume != identity.volume
        || actual.identity.file_id != identity.file_id
        || !security.permits_stage(&actual, private, after_commit_intent)
        || content != *proposed
    {
        return Err(CheckpointError::Invalid(
            "staging file no longer matches its record",
        ));
    }
    Ok(content.bytes)
}

/// Windows keeps a stage owner-only until its commit attempt.
#[cfg(windows)]
fn stage_is_private(file: &File) -> Result<bool, CheckpointError> {
    Ok(omgcode_platform::private_store::is_private_file(file)?)
}

/// Linux stages take their final mode before staging; privacy is not tracked.
#[cfg(not(windows))]
fn stage_is_private(_file: &File) -> Result<bool, CheckpointError> {
    Ok(false)
}

fn lock(file: &File) -> Result<(), CheckpointError> {
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => CheckpointError::Busy,
        std::fs::TryLockError::Error(error) => CheckpointError::Io(error),
    })
}

/// Count one observed file without overflow or credit for hypothetical cleanup.
fn add_usage(used: &mut u64, amount: u64) -> Result<(), CheckpointError> {
    *used = used.checked_add(amount).ok_or(CheckpointError::Limit)?;
    if *used > MAX_STORAGE {
        return Err(CheckpointError::Limit);
    }
    Ok(())
}

/// Refuse unless current evidence and the full reservation fit the aggregate cap.
fn ensure_budget(used: u64, reserve: u64) -> Result<(), CheckpointError> {
    if used
        .checked_add(reserve)
        .is_none_or(|total| total > MAX_STORAGE)
    {
        return Err(CheckpointError::Limit);
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use crate::policy::WorkspaceId;

    struct Fixture {
        _temporary: tempfile::TempDir,
        data: std::path::PathBuf,
        source: std::path::PathBuf,
        control: ControlStore,
        target: FileTarget,
    }

    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().expect("private fixture");
            let data = temporary.path().join("data");
            let control = ControlStore::open(&data).expect("control writer");
            let source = temporary.path().join("source.txt");
            fs::write(&source, b"original bytes\n").expect("source fixture");
            let target = FileTarget {
                session_id: Uuid::new_v4(),
                workspace_id: WorkspaceId::fresh(),
                path: "source.txt".into(),
            };
            Self {
                _temporary: temporary,
                data,
                source,
                control,
                target,
            }
        }

        fn root(&self) -> PrivateDirectory {
            open_root(&context(&self.control).expect("context").0).expect("protected root")
        }
        fn control_id(&self) -> Uuid {
            context(&self.control).expect("context").1
        }
        fn initialize(&self) -> Store {
            Store::initialize(self.root(), self.control_id(), Faults::default())
                .expect("initialize")
        }
        fn capture(&self) -> FileCapture {
            let metadata = fs::metadata(&self.source).expect("source metadata");
            FileCapture {
                bytes: fs::read(&self.source).expect("source bytes"),
                volume: metadata.dev(),
                file_id: u128::from(metadata.ino()).to_le_bytes(),
                security: omgcode_platform::private_store::NativeSecurity::Unix {
                    owner: metadata.uid(),
                    group: metadata.gid(),
                    mode: metadata.mode() & 0o777,
                },
            }
        }
        fn ledger_bytes(&self) -> Vec<u8> {
            fs::read(self.data.join(DIRECTORY).join(LEDGER)).expect("ledger evidence")
        }
    }

    #[test]
    fn initialization_failures_preserve_prefixes_without_repair_or_replay() {
        for point in [
            "initializing_sync",
            "store_creation",
            "header_sync",
            "ready_sync",
            "initializing_partial_write",
            "header_partial_write",
            "ready_partial_write",
        ] {
            let fixture = Fixture::new();
            let result = Store::initialize(
                fixture.root(),
                fixture.control_id(),
                Faults { at: Some(point) },
            );
            assert!(result.is_err(), "{point}");
            let before = fs::read(fixture.data.join(ANCHOR)).expect("anchor preserved");
            let view = inspect(&fixture.control);
            if point == "initializing_partial_write" || point == "ready_partial_write" {
                assert!(view.is_err(), "partial records cannot become ready");
            } else if point == "ready_sync" {
                assert_eq!(
                    view.expect("complete ready is visible").state,
                    CheckpointState::Ready
                );
            } else {
                assert_eq!(
                    view.expect("incomplete initialization evidence").state,
                    CheckpointState::Initializing
                );
                assert!(check_admission(&fixture.control).is_err());
            }
            assert_eq!(
                fs::read(fixture.data.join(ANCHOR)).expect("anchor unchanged"),
                before
            );
            assert_eq!(
                fs::read(&fixture.source).expect("source unchanged"),
                b"original bytes\n"
            );
        }
    }

    #[test]
    fn empty_or_orphaned_initialization_is_never_treated_as_unused() {
        for orphan_directory in [false, true] {
            let fixture = Fixture::new();
            if orphan_directory {
                fixture
                    .root()
                    .create_dir(OsStr::new(DIRECTORY))
                    .expect("orphan fixture");
            } else {
                fixture
                    .root()
                    .create_file(OsStr::new(ANCHOR))
                    .expect("empty anchor fixture");
            }
            let names = fs::read_dir(&fixture.data).expect("inventory").count();
            assert!(inspect(&fixture.control).is_err());
            assert!(check_admission(&fixture.control).is_err());
            assert!(
                Store::open_or_initialize(&fixture.control, fixture.capture().volume, |_| Ok(()))
                    .is_err()
            );
            assert_eq!(
                fs::read_dir(&fixture.data)
                    .expect("unchanged inventory")
                    .count(),
                names
            );
        }
    }

    #[test]
    fn prepared_state_is_not_inferred_from_files_after_a_failed_sync() {
        for point in [
            "intent_sync",
            "entry_creation",
            "original",
            "proposed",
            "manifest.json",
            "checkpoint_parent_sync",
            "prepared_sync",
        ] {
            let fixture = Fixture::new();
            let mut store = fixture.initialize();
            store.faults.at = Some(point);
            let error = store
                .prepare(&fixture.target, 0, &fixture.capture(), b"proposed bytes\n")
                .expect_err("injected failure");
            assert!(
                matches!(
                    error,
                    CheckpointError::Uncertain {
                        operation_id: Some(_),
                        ..
                    }
                ),
                "{point}"
            );
            assert!(store.poisoned);
            assert!(matches!(
                store.prepare(&fixture.target, 0, &fixture.capture(), b"retry"),
                Err(CheckpointError::Poisoned)
            ));
            if point == "intent_sync" {
                assert!(store.snapshot.operations.is_empty());
            } else {
                assert_eq!(
                    store
                        .snapshot
                        .pending()
                        .expect("prior in-memory intent")
                        .progress,
                    ReplacementProgress::Intent
                );
            }
            let before = fixture.ledger_bytes();
            drop(store);
            let view =
                inspect(&fixture.control).expect("strict reopen of complete visible records");
            assert!(view.pending.is_some());
            assert_eq!(
                view.operations[0].progress,
                if point == "prepared_sync" {
                    ReplacementProgress::CheckpointPrepared
                } else {
                    ReplacementProgress::Intent
                }
            );
            assert_eq!(view.operations[0].outcome, None);
            assert_eq!(
                fixture.ledger_bytes(),
                before,
                "inspection never appends or repairs"
            );
            assert_eq!(
                fs::read(&fixture.source).expect("source unchanged"),
                b"original bytes\n"
            );
        }
    }

    #[test]
    fn partial_ledger_writes_are_preserved_and_fail_closed_without_retry() {
        for point in ["intent_partial_write", "prepared_partial_write"] {
            let fixture = Fixture::new();
            let mut store = fixture.initialize();
            store.faults.at = Some(point);
            assert!(matches!(
                store.prepare(&fixture.target, 0, &fixture.capture(), b"proposal"),
                Err(CheckpointError::Uncertain { .. })
            ));
            assert!(store.poisoned);
            let bytes = fixture.ledger_bytes();
            assert!(!bytes.ends_with(b"\n"));
            drop(store);
            assert!(inspect(&fixture.control).is_err());
            assert!(check_admission(&fixture.control).is_err());
            assert_eq!(fixture.ledger_bytes(), bytes);
        }
    }

    #[test]
    fn acknowledgement_sync_failure_is_uncertain_and_does_not_publish_memory() {
        for point in ["ack_sync", "ack_partial_write"] {
            let fixture = Fixture::new();
            let mut store = fixture.initialize();
            let receipt = store
                .prepare(&fixture.target, 0, &fixture.capture(), b"proposal")
                .expect("prepared checkpoint");
            store.faults.at = Some(point);
            let error =
                record_acknowledgement(&mut store, receipt.operation_id).expect_err("ack failure");
            assert_eq!(error.code(), "checkpoint_uncertain");
            assert!(store.poisoned);
            assert!(store.snapshot.pending().is_some());
            let before = fixture.ledger_bytes();
            drop(store);
            let reopened = inspect(&fixture.control);
            if point == "ack_sync" {
                let view = reopened.expect("complete visible acknowledgement");
                assert!(view.pending.is_none());
                assert!(view.operations[0].acknowledged);
                assert_eq!(view.operations[0].outcome, None);
            } else {
                assert!(reopened.is_err());
            }
            assert_eq!(fixture.ledger_bytes(), before);
            assert_eq!(
                fs::read(&fixture.source).expect("source preserved"),
                b"original bytes\n"
            );
        }
    }

    #[test]
    fn initialized_missing_or_foreign_ledgers_are_not_recreated() {
        for foreign in [false, true] {
            let fixture = Fixture::new();
            let store = fixture.initialize();
            let scope = store.snapshot.scope;
            drop(store);
            let path = fixture.data.join(DIRECTORY).join(LEDGER);
            if foreign {
                let other = ReplacementScope::new(Uuid::new_v4(), scope.replacement_store_id())
                    .expect("foreign scope");
                fs::write(
                    &path,
                    encode(other, 0, LedgerAction::Created).expect("foreign header"),
                )
                .expect("foreign fixture");
            } else {
                fs::remove_file(&path).expect("simulate missing evidence");
            }
            let before = fs::read(&path).ok();
            assert!(inspect(&fixture.control).is_err());
            assert!(
                Store::open_or_initialize(&fixture.control, fixture.capture().volume, |_| Ok(()))
                    .is_err()
            );
            assert_eq!(fs::read(&path).ok(), before);
        }
    }

    #[test]
    fn late_authority_and_filesystem_checks_precede_initialization() {
        let fixture = Fixture::new();
        let denied = Store::open_or_initialize(&fixture.control, fixture.capture().volume, |_| {
            Err(CheckpointError::Invalid("authority changed after setup"))
        });
        assert!(denied.is_err());
        assert!(!fixture.data.join(ANCHOR).exists());
        assert!(!fixture.data.join(DIRECTORY).exists());
        let foreign_volume = fixture
            .capture()
            .volume
            .checked_add(1)
            .expect("fixture device range");
        assert!(Store::open_or_initialize(&fixture.control, foreign_volume, |_| Ok(())).is_err());
        assert!(!fixture.data.join(ANCHOR).exists());
    }

    #[test]
    fn journal_lock_and_changed_bytes_refuse_work_without_overwriting_evidence() {
        let fixture = Fixture::new();
        let mut store = fixture.initialize();
        assert!(matches!(
            inspect(&fixture.control),
            Err(CheckpointError::Busy)
        ));
        let mut tampered = fixture.ledger_bytes();
        let index = tampered
            .iter()
            .position(|byte| *byte == b'{')
            .expect("JSON object");
        tampered[index] = b'[';
        fs::write(fixture.data.join(DIRECTORY).join(LEDGER), &tampered)
            .expect("same-length mutation");
        assert!(
            store
                .prepare(&fixture.target, 0, &fixture.capture(), b"proposal")
                .is_err()
        );
        assert_eq!(fixture.ledger_bytes(), tampered);
    }

    #[test]
    fn quota_and_recovery_tail_use_checked_bounds() {
        assert!(ensure_budget(MAX_STORAGE - 4096, 4096).is_ok());
        assert!(matches!(
            ensure_budget(MAX_STORAGE - 4095, 4096),
            Err(CheckpointError::Limit)
        ));
        assert!(matches!(
            ensure_budget(u64::MAX, 1),
            Err(CheckpointError::Limit)
        ));
        let scope = ReplacementScope::new(Uuid::new_v4(), Uuid::new_v4()).expect("scope");
        let operation_id = Uuid::new_v4();
        let actions = [
            LedgerAction::StagePrepared {
                operation_id,
                identity: super::super::RecordedFileIdentity {
                    volume: u64::MAX,
                    file_id: [255; 16],
                },
                content: RecordedArtifact {
                    bytes: 1024 * 1024,
                    sha256: "f".repeat(64),
                },
            },
            LedgerAction::CommitIntended { operation_id },
            LedgerAction::ReplacementFinished {
                operation_id,
                outcome: super::super::ReplacementOutcome::Uncertain,
                code: Some("z".repeat(128)),
            },
            LedgerAction::ReplacementAcknowledged { operation_id },
        ];
        let mut total = 0;
        for action in actions {
            let envelope = Envelope {
                schema_version: 1,
                control_store_id: scope.control_store_id(),
                replacement_store_id: scope.replacement_store_id(),
                event_id: Uuid::new_v4(),
                sequence: 4095,
                timestamp: "9999-12-31T23:59:59.999999999Z".to_owned(),
                action,
            };
            let length = serde_json::to_vec(&envelope)
                .expect("worst-case tail")
                .len()
                + 1;
            assert!(length <= TAIL_RECORD_BYTES);
            total += length;
        }
        assert!(total < RECOVERY_BYTES);
    }

    #[test]
    fn stored_metadata_and_payload_privacy_are_rechecked_on_reopen() {
        let fixture = Fixture::new();
        let mut store = fixture.initialize();
        let receipt = store
            .prepare(&fixture.target, 0, &fixture.capture(), b"proposal")
            .expect("prepared");
        drop(store);
        let original = fixture
            .data
            .join(DIRECTORY)
            .join(receipt.operation_id.to_string())
            .join("original");
        fs::set_permissions(&original, fs::Permissions::from_mode(0o644))
            .expect("unsafe private mode fixture");
        assert!(inspect(&fixture.control).is_err());
        assert!(acknowledge(&fixture.control, receipt.operation_id).is_err());
        assert_eq!(
            fs::metadata(&original).expect("mode preserved").mode() & 0o777,
            0o644
        );
    }
}
