//! Replacement records and protected private checkpoint storage.
//!
//! The byte-input inspectors remain read-only. FileBroker checkpoint methods
//! add protected native storage and existing-only recovery. The authorized apply
//! path publishes one staged file. Explicit restore reuses that path, and
//! explicit deletion removes one resolved entry; replay never restores,
//! deletes, or repeats recorded work.

mod manifest;
mod replay;
pub(crate) mod store;
pub use store::{
    CheckpointDeletion, CheckpointError, CheckpointInspection, CheckpointReceipt, CheckpointState,
    ReplacementReceipt,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::policy::WorkspaceId;
use crate::session::NativePath;

/// A controlled format, scope, or capacity error. Display omits untrusted text.
#[derive(Debug, Error)]
pub enum ReplacementError {
    /// A record violates the versioned contract or its preceding state.
    #[error("invalid replacement records: {0}")]
    Invalid(&'static str),
    /// The supplied bytes or recorded resources exceed a fixed bound.
    #[error("replacement record limit reached: {0}")]
    Limit(&'static str),
    /// JSON syntax, duplicate keys, or a typed record shape is invalid.
    #[error("replacement record JSON is malformed, duplicated, or unsupported")]
    Json(#[source] serde_json::Error),
}

/// Expected control and replacement store IDs. These are metadata, not grants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplacementScope {
    control_store_id: Uuid,
    replacement_store_id: Uuid,
}

impl ReplacementScope {
    /// Validate two non-nil RFC 4122 version-4 IDs without opening storage.
    pub fn new(
        control_store_id: Uuid,
        replacement_store_id: Uuid,
    ) -> Result<Self, ReplacementError> {
        replay::validate_uuid(control_store_id)?;
        replay::validate_uuid(replacement_store_id)?;
        Ok(Self {
            control_store_id,
            replacement_store_id,
        })
    }

    /// Return the expected control-store identity.
    pub const fn control_store_id(self) -> Uuid {
        self.control_store_id
    }

    /// Return the expected replacement-store identity.
    pub const fn replacement_store_id(self) -> Uuid {
        self.replacement_store_id
    }
}

/// Last complete initialization claim. Neither variant proves persistence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitializationPhase {
    /// Initialization started and no complete ready record follows.
    Initializing,
    /// The journal records readiness; native artifacts still need validation.
    Ready,
}

/// Initialization evidence returned only after the entire supplied input validates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitializationSnapshot {
    /// Recorded store identities, checked against the expected control store.
    pub scope: ReplacementScope,
    /// Last complete recorded phase, not an authorization or durability result.
    pub phase: InitializationPhase,
    /// Number of accepted complete records, at most two.
    pub event_count: usize,
    /// Exact supplied byte count, including LF or CRLF delimiters.
    pub encoded_bytes: usize,
}

/// Furthest recorded preparation/commit step. This is historical evidence only.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplacementProgress {
    /// Intent exists; no prepared checkpoint has been recorded.
    Intent,
    /// An exact manifest reference has been recorded.
    CheckpointPrepared,
    /// A stage identity and matching proposed-content descriptor were recorded.
    StagePrepared,
    /// Commit intent exists; replacement may have occurred.
    CommitIntended,
}

/// Recorded terminal result. This enum cannot establish a native side effect.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplacementOutcome {
    /// Completion was recorded after commit intent.
    Succeeded,
    /// A clean failure was recorded before commit intent.
    Failed,
    /// A pre-commit conflict was recorded without publication.
    Conflict,
    /// An uncertain result still needs explicit acknowledgement.
    Uncertain,
}

/// Recorded lifecycle of an operation's private entry. Deletion is explicit.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointRetention {
    /// The private entry is retained; inspection validates its artifacts.
    Retained,
    /// Deletion was recorded and may be partial. Only the same explicit
    /// deletion request can finish it; nothing else is admitted meanwhile.
    Deleting,
    /// Deletion finished. The entry must be absent; its history remains.
    Deleted,
}

/// A bounded size/digest claim, not proof of the bytes of any file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedArtifact {
    /// Claimed exact byte count. The parser applies the relevant resource cap.
    pub bytes: u64,
    /// Claimed lowercase hexadecimal SHA-256 digest.
    pub sha256: String,
}

/// Fixed-width native identity metadata, not an opened object or access grant.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedFileIdentity {
    /// Claimed native volume identifier.
    pub volume: u64,
    /// Claimed native file ID, encoded in the platform's 16-byte representation.
    pub file_id: [u8; 16],
}

/// One retained operation's recorded claims. These fields are untrusted data;
/// constructing or changing a copy cannot affect storage or grant authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RecordedReplacement {
    /// Unique operation ID, also the fixed UUID-named private entry locator.
    pub operation_id: Uuid,
    /// Historical session association.
    pub session_id: Uuid,
    /// Historical workspace association, not a binding.
    pub workspace_id: WorkspaceId,
    /// Historical policy revision, not current authorization.
    pub revision: u64,
    /// Lossless nonempty relative path in this operating system's encoding.
    pub target: NativePath,
    /// Expected original-byte claim, bounded to 1 MiB.
    pub expected: RecordedArtifact,
    /// Proposed-byte claim, bounded to 1 MiB.
    pub proposed: RecordedArtifact,
    /// Exact manifest reference, if preparation was recorded.
    pub manifest: Option<RecordedArtifact>,
    /// Stage identity, if stage preparation was recorded.
    pub stage_identity: Option<RecordedFileIdentity>,
    /// Last recorded step, retained even after outcome or acknowledgement.
    pub progress: ReplacementProgress,
    /// Recorded outcome; acknowledgement never creates or changes one.
    pub outcome: Option<ReplacementOutcome>,
    /// Bounded machine-code text for non-success outcomes. It remains untrusted
    /// metadata and can contain sensitive text despite its restricted syntax.
    pub code: Option<String>,
    /// Whether the journal contains explicit acknowledgement of this operation.
    pub acknowledged: bool,
    /// Recorded private-entry lifecycle; deletion never changes the outcome.
    pub retention: CheckpointRetention,
}

/// Fully validated in-memory ledger evidence. No filesystem work is performed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplacementSnapshot {
    /// Expected store identities, matched by every record.
    pub scope: ReplacementScope,
    /// All recorded operations in order, including deleted entries' history.
    /// No history is evicted; at most 64 operations are not yet deleted.
    pub operations: Vec<RecordedReplacement>,
    /// Number of complete accepted records, including the header.
    pub event_count: usize,
    /// Exact supplied byte count, including record delimiters.
    pub encoded_bytes: usize,
    pending_operation_id: Option<Uuid>,
    pending_deletion_id: Option<Uuid>,
}

impl ReplacementSnapshot {
    /// Return unresolved recorded evidence, never permission to retry or repair.
    pub fn pending(&self) -> Option<&RecordedReplacement> {
        self.find(self.pending_operation_id?)
    }

    /// Return a recorded, unfinished deletion. Only the same explicit deletion
    /// request can finish it; this is not permission to delete anything else.
    pub fn pending_deletion(&self) -> Option<&RecordedReplacement> {
        self.find(self.pending_deletion_id?)
    }

    fn find(&self, id: Uuid) -> Option<&RecordedReplacement> {
        self.operations
            .iter()
            .find(|operation| operation.operation_id == id)
    }
}

/// Validate an existing initialization journal supplied as bytes.
///
/// Empty/partial/malformed input is an error, never an unused-store observation.
/// One complete initializing record is valid incomplete evidence. A ready claim
/// does not prove that the store, ledger, or directory entries exist or synced.
/// Input is at most 4 KiB and two records. No files are opened or changed.
///
/// ```
/// use omgcode_core::replacement::{InitializationPhase, inspect_initialization};
/// use uuid::Uuid;
///
/// let control = Uuid::new_v4();
/// let record = serde_json::json!({
///     "schema_version": 1,
///     "control_store_id": control,
///     "replacement_store_id": Uuid::new_v4(),
///     "event_id": Uuid::new_v4(),
///     "sequence": 0,
///     "timestamp": "2026-11-09T16:00:00Z",
///     "action": {"type": "initializing"}
/// });
/// let mut bytes = serde_json::to_vec(&record)?;
/// bytes.push(b'\n');
/// let evidence = inspect_initialization(&bytes, control)?;
/// assert_eq!(evidence.phase, InitializationPhase::Initializing);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn inspect_initialization(
    bytes: &[u8],
    expected_control_store_id: Uuid,
) -> Result<InitializationSnapshot, ReplacementError> {
    replay::initialization(bytes, expected_control_store_id)
}

/// Validate a complete bounded ledger and preserve every unfinished operation.
///
/// Limits are 8 MiB, 4,096 records, 64 KiB per record including its delimiter,
/// and 64 operation IDs whose entries are not deleted. Duplicate JSON keys and unknown fields fail.
/// Targets must use this OS's native encoding and broker-relative path syntax.
/// This function validates only recorded claims, not checkpoint files, native
/// identities, current policy, aggregate disk quota, durability, or authority.
pub fn inspect_ledger(
    bytes: &[u8],
    expected_scope: ReplacementScope,
) -> Result<ReplacementSnapshot, ReplacementError> {
    replay::ledger(bytes, expected_scope)
}
