//! Exact, bounded checkpoint manifests. They never authorize workspace access.

use omgcode_platform::broker::FileCapture;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::store::CheckpointError;
use super::{RecordedArtifact, RecordedFileIdentity, RecordedReplacement, ReplacementScope};
use crate::files::FileTarget;
use crate::session::{NativePath, sha256_hex};
use crate::storage::parse_strict_json;

pub(super) const MAX_MANIFEST: usize = 64 * 1024;

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

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "platform", rename_all = "snake_case", deny_unknown_fields)]
enum SecurityMetadata {
    Linux { owner: u32, group: u32, mode: u32 },
}

impl Manifest {
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
            security: SecurityMetadata::Linux {
                owner: capture.owner,
                group: capture.group,
                mode: capture.mode,
            },
        };
        let bytes = serde_json::to_vec(&manifest).map_err(CheckpointError::Json)?;
        if bytes.len() > MAX_MANIFEST {
            return Err(CheckpointError::Limit);
        }
        Ok(bytes)
    }

    pub(super) fn validate(
        bytes: &[u8],
        scope: ReplacementScope,
        operation: &RecordedReplacement,
        volume: u64,
    ) -> Result<(), CheckpointError> {
        if bytes.len() > MAX_MANIFEST {
            return Err(CheckpointError::Limit);
        }
        let manifest: Self =
            serde_json::from_value(parse_strict_json(bytes).map_err(CheckpointError::Json)?)
                .map_err(CheckpointError::Json)?;
        let SecurityMetadata::Linux { mode, .. } = manifest.security;
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
            || mode > 0o777
        {
            return Err(CheckpointError::Invalid(
                "manifest does not match its recorded operation",
            ));
        }
        Ok(())
    }
}

pub(super) fn descriptor(bytes: &[u8]) -> Result<RecordedArtifact, CheckpointError> {
    Ok(RecordedArtifact {
        bytes: u64::try_from(bytes.len()).map_err(|_| CheckpointError::Limit)?,
        sha256: sha256_hex(bytes),
    })
}
