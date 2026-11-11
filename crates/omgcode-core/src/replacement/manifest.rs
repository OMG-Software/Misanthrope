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

    /// Whether a retained stage's security is acceptable at this point.
    ///
    /// A Windows stage keeps the private store's owner-only DACL until the
    /// commit attempt copies the source security just before the rename. Before
    /// commit intent it must still be private. After commit intent an
    /// interrupted attempt can leave either state. Linux stages always match.
    pub(super) fn permits_stage(
        &self,
        stage: &StageMetadata,
        private: bool,
        after_commit_intent: bool,
    ) -> bool {
        match self {
            Self::Linux { .. } => self.matches_stage(stage),
            Self::Windows { .. } => private || (after_commit_intent && self.matches_stage(stage)),
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

#[cfg(test)]
mod tests {
    use omgcode_platform::private_store::PrivateFileIdentity;

    use super::*;

    const SOURCE: &str = "O:S-1-5-21-1G:S-1-5-21-1D:AI(A;ID;FA;;;S-1-5-21-1)(A;ID;FR;;;BU)";
    const PRIVATE: &str = "O:S-1-5-21-1G:S-1-5-21-1D:P(A;;FA;;;S-1-5-21-1)";

    fn windows(descriptor: &str) -> SecurityMetadata {
        SecurityMetadata::Windows {
            descriptor: descriptor.to_owned(),
            not_content_indexed: false,
        }
    }

    fn stage(descriptor: &str) -> StageMetadata {
        StageMetadata {
            identity: PrivateFileIdentity {
                volume: 1,
                file_id: [2; 16],
            },
            security: NativeSecurity::Windows {
                descriptor: descriptor.to_owned(),
                not_content_indexed: false,
            },
        }
    }

    #[test]
    fn a_windows_stage_must_stay_private_until_commit_intent() {
        let source = windows(SOURCE);
        assert!(source.permits_stage(&stage(PRIVATE), true, false));
        assert!(
            !source.permits_stage(&stage(SOURCE), false, false),
            "a stage carrying the source DACL before commit intent is refused"
        );
        assert!(source.permits_stage(&stage(PRIVATE), true, true));
        assert!(
            source.permits_stage(&stage(SOURCE), false, true),
            "an interrupted attempt can leave the source DACL after commit intent"
        );
        assert!(
            !source.permits_stage(&stage(&format!("{PRIVATE}(A;;FR;;;WD)")), false, true),
            "any other DACL is refused"
        );
    }

    #[test]
    fn a_linux_stage_must_match_its_source_at_every_point() {
        let source = SecurityMetadata::Linux {
            owner: 1000,
            group: 1000,
            mode: 0o640,
        };
        let matching = StageMetadata {
            identity: PrivateFileIdentity {
                volume: 1,
                file_id: [2; 16],
            },
            security: NativeSecurity::Unix {
                owner: 1000,
                group: 1000,
                mode: 0o640,
            },
        };
        let private = StageMetadata {
            security: NativeSecurity::Unix {
                owner: 1000,
                group: 1000,
                mode: 0o600,
            },
            ..matching.clone()
        };
        for after_commit_intent in [false, true] {
            assert!(source.permits_stage(&matching, false, after_commit_intent));
            assert!(!source.permits_stage(&private, true, after_commit_intent));
        }
    }
}
