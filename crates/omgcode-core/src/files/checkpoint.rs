//! Authorized private preparation and existing-only operator recovery.

use omgcode_platform::broker::{self, FileCapture, RelativePath};
use uuid::Uuid;

use super::{FileBroker, FileError, FileOperation, FileOutcome, FileTarget, validate_target};
use crate::policy::{Operation, PolicyRequest, SessionBinding};
use crate::replacement::store::{self, Store};
use crate::replacement::{
    CheckpointError, CheckpointInspection, CheckpointReceipt, RecordedArtifact,
};
use crate::session::sha256_hex;

impl FileBroker<'_> {
    /// Capture and persist a private checkpoint without replacing the target.
    ///
    /// Requires fresh read/write preferences and a live process-local binding.
    /// Linux captures the source identity/security metadata from the same pinned
    /// file as its bytes. Unsupported backends refuse before checkpoint I/O.
    /// Prepared state remains pending until trusted acknowledgement; this is
    /// not an apply token. No payload bytes appear in the returned receipt.
    ///
    /// Each payload is at most 1 MiB. Storage retains at most 64 intents and
    /// 64 MiB of managed file bytes, with capacity reserved before artifacts.
    /// A write or sync error can leave uncertain private evidence. Reopen for
    /// inspection; do not automatically retry or remove that evidence.
    pub fn checkpoint_prepare(
        &mut self,
        binding: &SessionBinding,
        target: &FileTarget,
        expected: &RecordedArtifact,
        proposed: &[u8],
    ) -> Result<CheckpointReceipt, FileError> {
        let path = validate_request(target, expected, proposed)?;
        check_permissions(self.control, binding, target)?;
        if !cfg!(target_os = "linux") {
            return Err(CheckpointError::Io(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "native checkpoint storage is not qualified on this platform",
            ))
            .into());
        }
        let (workspace, mut audit, read_id) =
            self.begin(binding, target, &path, FileOperation::Read, Operation::Read)?;
        let capture = match workspace.capture(&path, broker::MAX_FILE_BYTES) {
            Ok(capture) => capture,
            Err(source) => {
                audit.finish(read_id, FileOutcome::Failed)?;
                return Err(FileError::Native(source));
            }
        };
        audit.finish(read_id, FileOutcome::Succeeded)?;
        // Lock order is control, then one subordinate journal at a time. The
        // read outcome is durable before the checkpoint anchor can be locked.
        drop(audit);
        validate_capture(&capture, expected)?;
        if capture.bytes == proposed {
            return Err(CheckpointError::Unchanged.into());
        }
        // Root setup precedes the callback; initialization cannot start from an
        // old authorization result. The control writer remains borrowed here.
        let mut store = Store::open_or_initialize(self.control, capture.volume, |_| {
            check_permissions(self.control, binding, target)
        })?;
        check_permissions(self.control, binding, target)?;
        Ok(store.prepare(target, self.control.revision(), &capture, proposed)?)
    }

    /// Inspect existing protected metadata without creating files or opening any
    /// workspace target. No checkpoint contents are returned.
    /// Invalid records or missing required artifacts return an error without
    /// repair. A complete initializing anchor reports incomplete initialization.
    pub fn checkpoint_inspect(&mut self) -> Result<CheckpointInspection, FileError> {
        Ok(store::inspect(self.control)?)
    }

    /// Acknowledge one unresolved operation in existing storage. This does not
    /// assert replacement success, restore bytes, delete evidence, or grant access.
    /// Only the current pending operation ID is accepted. The acknowledgement
    /// is appended and synced; a write or sync failure can be uncertain and must
    /// be inspected on reopen without automatic retry.
    pub fn checkpoint_acknowledge(&mut self, operation_id: Uuid) -> Result<(), FileError> {
        Ok(store::acknowledge(self.control, operation_id)?)
    }
}

/// Require both permissions from the same live binding while control is locked.
/// Reuse at mutation boundaries so setup time cannot make an old check a grant.
pub(super) fn check_permissions(
    control: &crate::control::ControlStore,
    binding: &SessionBinding,
    target: &FileTarget,
) -> Result<(), CheckpointError> {
    for operation in [Operation::Read, Operation::Write] {
        let decision = control.check_file_policy(
            binding,
            &PolicyRequest {
                session_id: target.session_id,
                workspace_id: target.workspace_id,
                operation,
            },
        )?;
        if !decision.configured_allowed() {
            return Err(CheckpointError::Denied(decision.reason()));
        }
    }
    Ok(())
}

/// Check the complete proposed bytes and exact precondition before opening a target.
pub(super) fn validate_request(
    target: &FileTarget,
    expected: &RecordedArtifact,
    proposed: &[u8],
) -> Result<RelativePath, FileError> {
    if proposed.len() > broker::MAX_FILE_BYTES
        || expected.bytes > 1024 * 1024
        || expected.sha256.len() != 64
        || !expected
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || std::str::from_utf8(proposed).is_err()
    {
        return Err(FileError::Invalid(
            "checkpoint requires bounded UTF-8 and an exact SHA-256/size precondition",
        ));
    }
    validate_target(target, false)
}

/// Compare the bytes from the retained source before creating private evidence.
pub(super) fn validate_capture(
    capture: &FileCapture,
    expected: &RecordedArtifact,
) -> Result<(), FileError> {
    if u64::try_from(capture.bytes.len())
        .map_err(|_| FileError::Invalid("capture size overflow"))?
        != expected.bytes
        || sha256_hex(&capture.bytes) != expected.sha256
    {
        return Err(CheckpointError::Conflict.into());
    }
    if std::str::from_utf8(&capture.bytes).is_err() {
        return Err(FileError::Invalid("checkpoint source is not UTF-8"));
    }
    Ok(())
}
