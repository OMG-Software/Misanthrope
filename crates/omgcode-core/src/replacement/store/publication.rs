//! Ordered publication records under the retained checkpoint writer.

#[cfg(target_os = "linux")]
use std::ffi::OsStr;

#[cfg(target_os = "linux")]
use omgcode_platform::private_store::{PrivateDirectory, PrivateFileIdentity};
use serde::Serialize;
use uuid::Uuid;

#[cfg(target_os = "linux")]
use super::{
    CheckpointError, LedgerAction, Store, TAIL_RECORD_BYTES, add_usage, validate_prepared,
    validate_stage,
};
use crate::replacement::RecordedArtifact;
#[cfg(target_os = "linux")]
use crate::replacement::{
    RecordedFileIdentity, RecordedReplacement, ReplacementOutcome, ReplacementProgress,
};

/// Result of one completed apply request. No source or proposed bytes are exposed.
#[derive(Debug, Serialize)]
pub struct ReplacementReceipt {
    /// The retained operation ID; absent when the requested bytes were identical.
    pub operation_id: Option<Uuid>,
    /// Descriptor of the captured original bytes.
    pub original: RecordedArtifact,
    /// Descriptor of the complete proposed bytes.
    pub proposed: RecordedArtifact,
    /// Whether the request proposed different bytes.
    pub changed: bool,
    /// Whether publication and its terminal record completed successfully.
    pub applied: bool,
}

#[cfg(target_os = "linux")]
impl Store {
    /// Open the already recorded private entry for the pending prepared operation.
    pub(crate) fn stage_directory(
        &self,
        operation_id: Uuid,
    ) -> Result<PrivateDirectory, CheckpointError> {
        self.prepared_operation(operation_id)?;
        Ok(self
            .directory
            .open_dir(OsStr::new(&operation_id.to_string()))?)
    }

    /// Reopen the stage and checkpoint. Count the stage bytes before appending
    /// its identity and content evidence. Metadata must match the original.
    pub(crate) fn record_stage(
        &mut self,
        operation_id: Uuid,
        identity: PrivateFileIdentity,
    ) -> Result<(), CheckpointError> {
        let operation = self.prepared_operation(operation_id)?;
        let entry = self.stage_directory(operation_id)?;
        let manifest = operation.manifest.as_ref().ok_or(CheckpointError::Invalid(
            "prepared checkpoint lacks manifest",
        ))?;
        let security = validate_prepared(
            &entry,
            operation,
            self.snapshot.scope,
            self.root.identity()?.volume,
            manifest,
        )?;
        let identity = RecordedFileIdentity {
            volume: identity.volume,
            file_id: identity.file_id,
        };
        let content = operation.proposed.clone();
        let length = validate_stage(&entry, &identity, &content, security)?;
        add_usage(&mut self.used, length)?;
        self.append(
            LedgerAction::StagePrepared {
                operation_id,
                identity,
                content,
            },
            3,
            3 * TAIL_RECORD_BYTES,
            "stage_prepared_sync",
        )
    }

    /// Persist the intent before the caller may make its single native attempt.
    pub(crate) fn record_commit_intent(
        &mut self,
        operation_id: Uuid,
    ) -> Result<(), CheckpointError> {
        self.append(
            LedgerAction::CommitIntended { operation_id },
            2,
            2 * TAIL_RECORD_BYTES,
            "commit_intent_sync",
        )
    }

    /// Record an observed pre-commit conflict without removing private evidence.
    pub(crate) fn finish_conflict(&mut self, operation_id: Uuid) -> Result<(), CheckpointError> {
        self.finish_publication(
            operation_id,
            ReplacementOutcome::Conflict,
            Some("target_changed"),
        )
    }

    /// Publish success only after native verification and both parent syncs.
    /// Revalidate retained checkpoints and account for the moved stage first.
    pub(crate) fn finish_publication_success(
        &mut self,
        operation_id: Uuid,
    ) -> Result<(), CheckpointError> {
        self.refresh_usage()?;
        self.finish_publication(operation_id, ReplacementOutcome::Succeeded, None)
    }

    /// Preserve the last visible history on failure. No later write on this
    /// handle may retry an append, publish a stage, or infer a terminal outcome.
    pub(crate) fn with_publication_evidence<T>(
        &mut self,
        operation_id: Uuid,
        action: impl FnOnce(&mut Self) -> Result<T, CheckpointError>,
    ) -> Result<T, CheckpointError> {
        if self.poisoned {
            return Err(CheckpointError::Poisoned);
        }
        action(self).map_err(|source| {
            self.poisoned = true;
            CheckpointError::Uncertain {
                operation_id: Some(operation_id),
                source: Box::new(source),
            }
        })
    }

    /// Instance-local hooks can interrupt publication at real ordering boundaries.
    pub(crate) fn publication_boundary(&mut self, name: &str) -> Result<(), CheckpointError> {
        Ok(self.faults.check(name)?)
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn inject_publication_fault(&mut self, name: &'static str) {
        self.faults.at = Some(name);
    }

    fn prepared_operation(
        &self,
        operation_id: Uuid,
    ) -> Result<&RecordedReplacement, CheckpointError> {
        self.snapshot
            .pending()
            .filter(|operation| {
                operation.operation_id == operation_id
                    && operation.progress == ReplacementProgress::CheckpointPrepared
                    && operation.outcome.is_none()
            })
            .ok_or(CheckpointError::WrongOperation)
    }

    fn finish_publication(
        &mut self,
        operation_id: Uuid,
        outcome: ReplacementOutcome,
        code: Option<&str>,
    ) -> Result<(), CheckpointError> {
        self.append(
            LedgerAction::ReplacementFinished {
                operation_id,
                outcome,
                code: code.map(str::to_owned),
            },
            1,
            TAIL_RECORD_BYTES,
            "publication_outcome_sync",
        )
    }
}
