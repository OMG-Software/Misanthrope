//! Exact, bounded checkpoint manifests. They never authorize workspace access.

use omgcode_platform::broker::FileCapture;
use omgcode_platform::private_store::{NativeSecurity, StageMetadata};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::store::CheckpointError;
use super::{RecordedArtifact, RecordedFileIdentity, RecordedReplacement, ReplacementScope};
use crate::files::FileTarget;
use crate::session::{NativePath, sha256_hex};
use crate::storage::parse_strict_json;

pub(super) const MAX_MANIFEST: usize = 64 * 1024;
const MAX_DESCRIPTOR: usize = 16 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    schema_version: u32,
    control_store_id: Uuid,
    replacement_store_id: Uuid,
    operation_id: Uuid,
    session_id: Uuid,
    workspace_id: Uuid,
    revision: u64,
    target: NativePath,
    original: RecordedArtifact,
    proposed: RecordedArtifact,
    source: RecordedFileIdentity,
    security: SecurityMetadata,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "platform", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum SecurityMetadata {
    Linux {
        owner: u32,
        group: u32,
        mode: u32,
    },
    Windows {
        descriptor: String,
        not_content_indexed: bool,
    },
}

impl SecurityMetadata {
    fn from_native(security: &NativeSecurity) -> Self {
        match security {
            NativeSecurity::Unix { owner, group, mode } => Self::Linux {
                owner: *owner,
                group: *group,
                mode: *mode,
            },
            NativeSecurity::Windows {
                descriptor,
                not_content_indexed,
            } => Self::Windows {
                descriptor: descriptor.clone(),
                not_content_indexed: *not_content_indexed,
            },
        }
    }

    /// Match the metadata that the new file must preserve from the captured source.
    pub(super) fn matches_stage(&self, stage: &StageMetadata) -> bool {
        match (self, &stage.security) {
            (
                Self::Linux { owner, group, mode },
                NativeSecurity::Unix {
                    owner: stage_owner,
                    group: stage_group,
                    mode: stage_mode,
                },
            ) => (owner, group, mode) == (stage_owner, stage_group, stage_mode),
            (
                Self::Windows {
                    descriptor,
                    not_content_indexed,
                },
                NativeSecurity::Windows {
                    descriptor: stage_descriptor,
                    not_content_indexed: stage_not_content_indexed,
                },
            ) => (descriptor, not_content_indexed) == (stage_descriptor, stage_not_content_indexed),
            _ => false,
        }
    }

    /// Check the recorded shape. This does not prove the claim is current.
    fn is_well_formed(&self) -> bool {
        match self {
            Self::Linux { mode, .. } => *mode <= 0o777,
            Self::Windows { descriptor, .. } => {
                !descriptor.is_empty()
                    && descriptor.len() <= MAX_DESCRIPTOR
                    && descriptor.is_ascii()
            }
        }
    }
}

impl Manifest {
    /// Bind exact payload descriptors and same-handle source metadata to this
    /// intent. This records evidence only; it never reopens or authorizes a target.
    pub(super) fn encode(
        scope: ReplacementScope,
        operation_id: Uuid,
        target: &FileTarget,
        revision: u64,
        capture: &FileCapture,
        proposed: &[u8],
    ) -> Result<Vec<u8>, CheckpointError> {
        let manifest = Self {
            schema_version: 1,
            control_store_id: scope.control_store_id(),
            replacement_store_id: scope.replacement_store_id(),
            operation_id,
            session_id: target.session_id,
            workspace_id: *target.workspace_id.as_uuid(),
            revision,
            target: NativePath::from_path(&target.path)
                .map_err(|_| CheckpointError::Invalid("native manifest target"))?,
            original: descriptor(&capture.bytes)?,
            proposed: descriptor(proposed)?,
            source: RecordedFileIdentity {
                volume: capture.volume,
                file_id: capture.file_id,
            },
            security: SecurityMetadata::from_native(&capture.security),
        };
        let bytes = serde_json::to_vec(&manifest).map_err(CheckpointError::Json)?;
        if bytes.len() > MAX_MANIFEST {
            return Err(CheckpointError::Limit);
        }
        Ok(bytes)
    }

    /// Reject unknown or duplicate fields and mismatches with the strict ledger.
    /// The caller must also compare the encoded manifest digest and actual files;
    /// source metadata is a captured claim, not a check of the current workspace.
    pub(super) fn validate(
        bytes: &[u8],
        scope: ReplacementScope,
        operation: &RecordedReplacement,
        volume: u64,
    ) -> Result<SecurityMetadata, CheckpointError> {
        if bytes.len() > MAX_MANIFEST {
            return Err(CheckpointError::Limit);
        }
        let manifest: Self =
            serde_json::from_value(parse_strict_json(bytes).map_err(CheckpointError::Json)?)
                .map_err(CheckpointError::Json)?;
        if manifest.schema_version != 1
            || manifest.control_store_id != scope.control_store_id()
            || manifest.replacement_store_id != scope.replacement_store_id()
            || manifest.operation_id != operation.operation_id
            || manifest.session_id != operation.session_id
            || manifest.workspace_id != *operation.workspace_id.as_uuid()
            || manifest.revision != operation.revision
            || manifest.target != operation.target
            || manifest.original != operation.expected
            || manifest.proposed != operation.proposed
            || manifest.source.volume != volume
            || !manifest.security.is_well_formed()
        {
            return Err(CheckpointError::Invalid(
                "manifest does not match its recorded operation",
            ));
        }
        Ok(manifest.security)
    }
}

pub(super) fn descriptor(bytes: &[u8]) -> Result<RecordedArtifact, CheckpointError> {
    Ok(RecordedArtifact {
        bytes: u64::try_from(bytes.len()).map_err(|_| CheckpointError::Limit)?,
        sha256: sha256_hex(bytes),
    })
}
