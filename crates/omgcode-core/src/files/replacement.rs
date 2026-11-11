//! Authorized, single-attempt replacement of an existing workspace file.

use std::io;

#[cfg(any(target_os = "linux", windows))]
use omgcode_platform::broker;

#[cfg(any(target_os = "linux", windows))]
use super::checkpoint::{check_permissions, validate_capture, validate_request};
use super::{FileBroker, FileError, FileTarget};
#[cfg(any(target_os = "linux", windows))]
use super::{FileOperation, FileOutcome};
#[cfg(any(target_os = "linux", windows))]
use crate::policy::Operation;
use crate::policy::SessionBinding;
#[cfg(any(target_os = "linux", windows))]
use crate::replacement::store::{self, Store};
use crate::replacement::{CheckpointError, RecordedArtifact, ReplacementReceipt};
use uuid::Uuid;

#[cfg(all(test, any(target_os = "linux", windows)))]
mod tests;

impl FileBroker<'_> {
    /// Apply complete UTF-8 content against an exact size and SHA-256 precondition.
    ///
    /// A trusted caller must stop independent writers. The final check and rename
    /// are separate operations; this API cannot guarantee no lost updates. It
    /// requires a live binding and both read and write preferences. Unsupported
    /// platforms, mounts, or metadata fail closed. No recorded history is replayed.
    ///
    /// A write, commit, verification, or sync failure can leave private evidence
    /// and a changed target. Inspect the operation before any further decision.
    /// This method never retries a commit, restores bytes, or removes evidence.
    pub fn apply_replacement(
        &mut self,
        binding: &SessionBinding,
        target: &FileTarget,
        expected: &RecordedArtifact,
        proposed: &[u8],
    ) -> Result<ReplacementReceipt, FileError> {
        #[cfg(any(target_os = "linux", windows))]
        {
            self.apply_with_preparation_hook(binding, target, expected, proposed, |_| {})
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let _ = (binding, target, expected, proposed);
            Err(CheckpointError::Io(io::Error::new(
                io::ErrorKind::Unsupported,
                "native replacement is not qualified on this platform",
            ))
            .into())
        }
    }

    /// Restore one retained checkpoint's original bytes through the apply path.
    ///
    /// `expected` describes the *current* target, never the checkpoint: a later
    /// edit causes a controlled conflict, not an overwrite. The checkpoint must
    /// be prepared, retained, and recorded for this workspace and exact native
    /// target path; its bytes are reread and checked against the record. The
    /// current bytes are then checkpointed again as a new operation, under the
    /// same live-binding, permission, stopped-writers, and no-CAS contract as
    /// `apply_replacement`. A missing target is refused; there is no
    /// restore-as-create. Nothing restores automatically.
    pub fn restore_checkpoint(
        &mut self,
        binding: &SessionBinding,
        target: &FileTarget,
        checkpoint: Uuid,
        expected: &RecordedArtifact,
    ) -> Result<ReplacementReceipt, FileError> {
        #[cfg(any(target_os = "linux", windows))]
        {
            validate_request(target, expected, &[])?;
            check_permissions(self.control, binding, target)?;
            // The store opens read-only and closes before apply reopens it, so
            // the lock order stays control, then one subordinate journal.
            let original = store::read_original(self.control, checkpoint, target)?;
            self.apply_with_preparation_hook(binding, target, expected, &original, |_| {})
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let _ = (binding, target, checkpoint, expected);
            Err(CheckpointError::Io(io::Error::new(
                io::ErrorKind::Unsupported,
                "native restore is not qualified on this platform",
            ))
            .into())
        }
    }

    /// The test hook runs after checkpoint sync and before staging. Production
    /// uses a no-op. The control writer remains held for the entire operation.
    #[cfg(any(target_os = "linux", windows))]
    fn apply_with_preparation_hook(
        &mut self,
        binding: &SessionBinding,
        target: &FileTarget,
        expected: &RecordedArtifact,
        proposed: &[u8],
        after_preparation: impl FnOnce(&mut Store),
    ) -> Result<ReplacementReceipt, FileError> {
        let path = validate_request(target, expected, proposed)?;
        check_permissions(self.control, binding, target)?;
        let (workspace, mut audit, read_id) =
            self.begin(binding, target, &path, FileOperation::Read, Operation::Read)?;
        let native = match workspace.replacement_target(&path, broker::MAX_FILE_BYTES) {
            Ok(target) => target,
            Err(source) => {
                audit.finish(read_id, FileOutcome::Failed)?;
                return Err(FileError::Native(source));
            }
        };
        audit.finish(read_id, FileOutcome::Succeeded)?;
        // Control, then at most one subordinate journal. The workspace and its
        // retained target stay alive after the legacy read journal closes.
        drop(audit);

        let capture = native.capture();
        validate_capture(capture, expected)?;
        if capture.bytes == proposed {
            return Ok(ReplacementReceipt {
                operation_id: None,
                original: expected.clone(),
                proposed: expected.clone(),
                changed: false,
                applied: false,
            });
        }

        let mut store = Store::open_or_initialize(self.control, capture.volume, |root| {
            native.check_storage(root)?;
            check_permissions(self.control, binding, target)
        })?;
        check_permissions(self.control, binding, target)?;
        let prepared = store.prepare(target, self.control.revision(), capture, proposed)?;
        let operation_id = prepared.operation_id;
        after_preparation(&mut store);

        let mut stage = store.with_publication_evidence(operation_id, |store| {
            check_permissions(self.control, binding, target)?;
            let directory = store.stage_directory(operation_id)?;
            let stage = native.prepare_stage(directory, proposed)?;
            store.record_stage(operation_id, stage.identity()?)?;
            Ok(stage)
        })?;
        let same_target =
            store.with_publication_evidence(operation_id, |_| Ok(native.recheck(&stage)?))?;
        if !same_target {
            store.with_publication_evidence(operation_id, |store| {
                store.finish_conflict(operation_id)
            })?;
            return Err(CheckpointError::Conflict.into());
        }

        store.with_publication_evidence(operation_id, |store| {
            check_permissions(self.control, binding, target)?;
            store.record_commit_intent(operation_id)?;
            // Sync can consume the binding lifetime. A denial here remains
            // unresolved under the v1 commit-intent contract; no rename follows.
            check_permissions(self.control, binding, target)?;
            store.publication_boundary("before_native_commit")?;
            native.commit(&mut stage, || {
                check_permissions(self.control, binding, target).map_err(io::Error::other)
            })?;
            store.publication_boundary("after_native_commit")?;
            store.finish_publication_success(operation_id)
        })?;

        Ok(ReplacementReceipt {
            operation_id: Some(operation_id),
            original: prepared.original,
            proposed: prepared.proposed,
            changed: true,
            applied: true,
        })
    }
}
