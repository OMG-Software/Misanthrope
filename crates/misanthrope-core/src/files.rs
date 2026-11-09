//! Authorized workspace file operations. Model tool dispatch is not implemented.
//!
//! Each operation uses a live binding, current policy and native directory
//! handles. Diagnostic policy results and serialized metadata are never grants.

use std::io;
use std::path::PathBuf;

use misanthrope_platform::broker::{self, RelativePath, Workspace};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::control::{ControlError, ControlStore};
use crate::file_audit::{FileAudit, FileAuditError, FileOperation, FileOutcome};
use crate::policy::{DecisionReason, Operation, PolicyRequest, SessionBinding, WorkspaceId};
use crate::session::{NativePath, SessionError};

/// Explicit request scope. IDs and path arguments do not establish authority.
#[derive(Debug)]
pub struct FileTarget {
    /// Session making this request; it must match the live binding.
    pub session_id: Uuid,
    /// Workspace being accessed; it must match the live binding.
    pub workspace_id: WorkspaceId,
    /// Native relative path. Empty is permitted for listing the workspace only.
    pub path: PathBuf,
}

/// A broker failure. An uncertain create must be inspected before acknowledgement.
#[derive(Debug, Error)]
pub enum FileError {
    /// Trusted configuration or its journal could not be validated.
    #[error("workspace control failed: {0}")]
    Control(#[from] ControlError),
    /// No access is permitted by the current binding and settings.
    #[error("file operation denied: {0:?}")]
    Denied(DecisionReason),
    /// Native names, limits, or scope are invalid.
    #[error("invalid file request: {0}")]
    Invalid(&'static str),
    /// The opened object or required native control could not be used.
    #[error("native file operation failed: {0}")]
    Native(#[source] io::Error),
    /// An existing entry prevents exclusive creation.
    #[error("entry already exists; overwriting is not supported")]
    Conflict,
    /// Native path metadata cannot be represented on this host.
    #[error("native path encoding failed: {0}")]
    Encoding(#[from] SessionError),
    /// Audit validation or durability failed; read data is withheld.
    #[error("file operation audit failed: {0}")]
    Audit(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// Recovery requires an explicit trusted acknowledgement of this operation.
    #[error("file operation {operation_id} is unresolved; inspect it before acknowledgement")]
    Pending { operation_id: Uuid },
    /// A new entry may exist even though the operation could not be acknowledged.
    #[error("file creation {operation_id} has an uncertain outcome: {source}")]
    Uncertain {
        /// Durable operation identifier used for inspection and acknowledgement.
        operation_id: Uuid,
        /// Original mutation or audit failure.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl From<FileAuditError> for FileError {
    /// Preserve the pending ID while keeping the private journal implementation internal.
    fn from(error: FileAuditError) -> Self {
        match error {
            FileAuditError::PendingOperation(operation_id) => Self::Pending { operation_id },
            other => Self::Audit(Box::new(other)),
        }
    }
}

/// A bounded, audited file read. Contents remain untrusted workspace data.
#[derive(Debug, Serialize)]
pub struct FileRead {
    /// Durable operation ID, never an access token.
    pub operation_id: Uuid,
    /// Workspace from the verified request scope.
    pub workspace_id: WorkspaceId,
    /// Lossless relative target metadata.
    pub target: NativePath,
    /// Exact bytes read; no executable interpretation or role promotion occurs.
    pub bytes: Vec<u8>,
    /// Digest of the returned bytes, not a reusable expected-version grant.
    pub sha256: String,
}

/// Descriptive directory entry. Every later read is independently authorized.
#[derive(Debug, Serialize)]
pub struct FileEntry {
    /// Lossless native single-component name.
    pub name: NativePath,
    /// Observed kind: file, directory, or blocked.
    pub kind: &'static str,
}

/// A bounded listing; concurrent changes can make entries stale immediately.
#[derive(Debug, Serialize)]
pub struct FileListing {
    /// Durable operation identifier.
    pub operation_id: Uuid,
    /// Workspace from the verified request scope.
    pub workspace_id: WorkspaceId,
    /// Relative listed directory; empty denotes the workspace.
    pub target: NativePath,
    /// Native entry names and observed kinds.
    pub entries: Vec<FileEntry>,
    /// The entry or aggregate name limit was reached.
    pub truncated: bool,
}

/// A successfully audited exclusive creation. It does not promise power-loss durability.
#[derive(Debug, Serialize)]
pub struct FileCreated {
    /// Durable operation identifier.
    pub operation_id: Uuid,
    /// Workspace from the verified request scope.
    pub workspace_id: WorkspaceId,
    /// Relative newly created target.
    pub target: NativePath,
    /// Bytes supplied for a new file; zero for a new directory.
    pub bytes_written: usize,
}

/// Metadata for trusted inspection. An unresolved intent never triggers action replay.
#[derive(Debug, Serialize)]
pub struct FileAuditView {
    /// Pending creation or interrupted operation, if acknowledgement is needed.
    pub pending: Option<Uuid>,
    /// Validated journal entries encoded as descriptive JSON.
    pub entries: Vec<serde_json::Value>,
}

/// Synchronous broker holding the control writer while it checks and performs I/O.
///
/// This serializes Misanthrope settings changes with each bounded operation.
/// It does not contain a separately running process or hostile same-user code.
pub struct FileBroker<'a> {
    control: &'a mut ControlStore,
    #[cfg(test)]
    fail_outcome_sync: bool,
}

impl<'a> FileBroker<'a> {
    /// Borrow the trusted control writer. No permission is granted by construction.
    pub fn new(control: &'a mut ControlStore) -> Self {
        Self {
            control,
            #[cfg(test)]
            fail_outcome_sync: false,
        }
    }

    /// Read a regular, single-link file and release bytes only after audit sync.
    pub fn read(
        &mut self,
        binding: &SessionBinding,
        target: &FileTarget,
        max_bytes: usize,
    ) -> Result<FileRead, FileError> {
        if !(1..=broker::MAX_FILE_BYTES).contains(&max_bytes) {
            return Err(FileError::Invalid("invalid read limit"));
        }
        let path = validate_target(target, false)?;
        let (workspace, mut audit, operation_id) =
            self.begin(binding, target, &path, FileOperation::Read, Operation::Read)?;
        let bytes = match workspace.read(&path, max_bytes) {
            Ok(bytes) => bytes,
            Err(error) => {
                audit.finish(operation_id, FileOutcome::Failed)?;
                return Err(FileError::Native(error));
            }
        };
        let sha256 = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        audit.finish(operation_id, FileOutcome::Succeeded)?;
        Ok(FileRead {
            operation_id,
            workspace_id: target.workspace_id,
            target: NativePath::from_path(&path.to_path_buf())?,
            bytes,
            sha256,
        })
    }

    /// List one directory without interpreting names or following listed links.
    pub fn list(
        &mut self,
        binding: &SessionBinding,
        target: &FileTarget,
        max_entries: usize,
    ) -> Result<FileListing, FileError> {
        if !(1..=broker::MAX_DIRECTORY_ENTRIES).contains(&max_entries) {
            return Err(FileError::Invalid("invalid entry limit"));
        }
        let path = validate_target(target, true)?;
        let (workspace, mut audit, operation_id) =
            self.begin(binding, target, &path, FileOperation::List, Operation::Read)?;
        let listing = match workspace.list(&path, max_entries, broker::MAX_DIRECTORY_NAME_UNITS) {
            Ok(listing) => listing,
            Err(error) => {
                audit.finish(operation_id, FileOutcome::Failed)?;
                return Err(FileError::Native(error));
            }
        };
        let entries = listing
            .entries
            .into_iter()
            .map(|entry| {
                Ok(FileEntry {
                    name: NativePath::from_path(&PathBuf::from(entry.name))?,
                    kind: match entry.kind {
                        broker::EntryKind::File => "file",
                        broker::EntryKind::Directory => "directory",
                        broker::EntryKind::Blocked => "blocked",
                    },
                })
            })
            .collect::<Result<Vec<_>, SessionError>>()?;
        audit.finish(operation_id, FileOutcome::Succeeded)?;
        Ok(FileListing {
            operation_id,
            workspace_id: target.workspace_id,
            target: NativePath::from_path(&path.to_path_buf())?,
            entries,
            truncated: listing.truncated,
        })
    }

    /// Create one new file. An existing entry is never replaced, even on collision.
    pub fn create_file(
        &mut self,
        binding: &SessionBinding,
        target: &FileTarget,
        bytes: &[u8],
    ) -> Result<FileCreated, FileError> {
        if bytes.len() > broker::MAX_FILE_BYTES {
            return Err(FileError::Invalid("file exceeds 1 MiB"));
        }
        let path = validate_target(target, false)?;
        let (workspace, mut audit, operation_id) = self.begin(
            binding,
            target,
            &path,
            FileOperation::CreateFile,
            Operation::Write,
        )?;
        complete_creation(&mut audit, operation_id, workspace.create_new(&path, bytes))?;
        Ok(FileCreated {
            operation_id,
            workspace_id: target.workspace_id,
            target: NativePath::from_path(&path.to_path_buf())?,
            bytes_written: bytes.len(),
        })
    }

    /// Create one new directory with an existing, independently opened parent.
    pub fn create_directory(
        &mut self,
        binding: &SessionBinding,
        target: &FileTarget,
    ) -> Result<FileCreated, FileError> {
        let path = validate_target(target, false)?;
        let (workspace, mut audit, operation_id) = self.begin(
            binding,
            target,
            &path,
            FileOperation::CreateDirectory,
            Operation::Write,
        )?;
        complete_creation(&mut audit, operation_id, workspace.create_dir(&path))?;
        Ok(FileCreated {
            operation_id,
            workspace_id: target.workspace_id,
            target: NativePath::from_path(&path.to_path_buf())?,
            bytes_written: 0,
        })
    }

    /// Inspect private operation metadata through a trusted operator interface.
    pub fn audit(&mut self, limit: usize) -> Result<FileAuditView, FileError> {
        let (data_root, store_id) = self.control.file_audit_context()?;
        let audit = FileAudit::open(&data_root, store_id)?;
        let entries = audit
            .entries(limit)?
            .into_iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| FileError::Audit(Box::new(error)))?;
        Ok(FileAuditView {
            pending: audit.pending(),
            entries,
        })
    }

    /// Record that a user inspected an uncertain operation. This retries no I/O.
    pub fn acknowledge(&mut self, operation_id: Uuid) -> Result<(), FileError> {
        let (data_root, store_id) = self.control.file_audit_context()?;
        FileAudit::open(&data_root, store_id)?.acknowledge(operation_id)?;
        Ok(())
    }

    /// Bind current policy to verified native handles and a durable operation intent.
    fn begin(
        &mut self,
        binding: &SessionBinding,
        target: &FileTarget,
        path: &RelativePath,
        operation: FileOperation,
        permission: Operation,
    ) -> Result<(Workspace, FileAudit, Uuid), FileError> {
        let decision = self.control.evaluate(
            binding,
            &PolicyRequest {
                session_id: target.session_id,
                workspace_id: target.workspace_id,
                operation: permission,
            },
        )?;
        if !decision.configured_allowed() {
            return Err(FileError::Denied(decision.reason()));
        }
        let scope = self.control.file_scope(target.workspace_id)?;
        let root_path = scope.root.live_path().map_err(FileError::Native)?;
        let workspace_path = scope.workspace.live_path().map_err(FileError::Native)?;
        // This derives a candidate name only. Native rooted opens and opened
        // object identity/creation checks below establish the filesystem scope.
        let relative = workspace_path
            .strip_prefix(&root_path)
            .map_err(|_| FileError::Invalid("workspace name cannot be derived beneath root"))?;
        let relative = RelativePath::new(relative).map_err(FileError::Native)?;
        let workspace = Workspace::open(
            &root_path,
            scope.root.native_identity(),
            &relative,
            scope.workspace.native_identity(),
        )
        .map_err(FileError::Native)?;
        let (root_created, workspace_created) =
            workspace.creation_times().map_err(FileError::Native)?;
        if !scope.root.matches_created(root_created)
            || !scope.workspace.matches_created(workspace_created)
        {
            return Err(FileError::Control(ControlError::DirectoryChanged));
        }
        let (data_root, store_id) = self.control.file_audit_context()?;
        let mut audit = FileAudit::open(&data_root, store_id)?;
        let operation_id = audit.start(
            target.session_id,
            target.workspace_id,
            self.control.revision(),
            operation,
            NativePath::from_path(&path.to_path_buf())?,
        )?;
        // Native setup and audit sync can take time. Recheck the live lease
        // immediately before admitting the target operation; no persisted
        // policy result is reused as an executor permit.
        let current = crate::policy::evaluate(
            store_id,
            self.control.revision(),
            self.control
                .workspace(target.workspace_id)
                .map(|workspace| workspace.permissions()),
            binding,
            &PolicyRequest {
                session_id: target.session_id,
                workspace_id: target.workspace_id,
                operation: permission,
            },
        );
        if !current.configured_allowed() {
            audit.finish(operation_id, FileOutcome::Failed)?;
            return Err(FileError::Denied(current.reason()));
        }
        #[cfg(test)]
        if std::mem::take(&mut self.fail_outcome_sync) {
            audit.fail_next_sync_for_test();
        }
        Ok((workspace, audit, operation_id))
    }
}

/// Pure native-name checks precede any access; only listing can target the root.
fn validate_target(target: &FileTarget, allow_root: bool) -> Result<RelativePath, FileError> {
    let path = RelativePath::new(&target.path).map_err(FileError::Native)?;
    if path.is_empty() && !allow_root {
        return Err(FileError::Invalid("target entry is required"));
    }
    Ok(path)
}

/// Keep uncertain mutations pending and never report success before durable audit.
fn complete_creation(
    audit: &mut FileAudit,
    operation_id: Uuid,
    result: Result<(), broker::CreateError>,
) -> Result<(), FileError> {
    match result {
        Ok(()) => audit
            .finish(operation_id, FileOutcome::Succeeded)
            .map_err(|source| FileError::Uncertain {
                operation_id,
                source: Box::new(source),
            }),
        Err(error) if error.may_have_created => {
            if let Err(source) = audit.finish(operation_id, FileOutcome::Uncertain) {
                return Err(FileError::Uncertain {
                    operation_id,
                    source: Box::new(source),
                });
            }
            Err(FileError::Uncertain {
                operation_id,
                source: Box::new(error.error),
            })
        }
        Err(error) => {
            audit.finish(operation_id, FileOutcome::Failed)?;
            if error.error.kind() == io::ErrorKind::AlreadyExists {
                Err(FileError::Conflict)
            } else {
                Err(FileError::Native(error.error))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    /// Keep app state outside the project and return a real current session binding.
    fn fixture() -> (
        tempfile::TempDir,
        ControlStore,
        SessionBinding,
        FileTarget,
        PathBuf,
    ) {
        let temporary = tempfile::tempdir().expect("private temporary directory");
        let data = temporary.path().join("data");
        let root = temporary.path().join("project");
        fs::create_dir(&root).expect("project");
        let mut control = ControlStore::open(&data).expect("control store");
        control
            .set_development_root(&root, 0)
            .expect("root setting");
        let workspace = control
            .register_workspace(&root, 1)
            .expect("registered workspace");
        let session_id = Uuid::new_v4();
        let binding = control
            .bind_session(session_id, workspace.id())
            .expect("current binding");
        let target = FileTarget {
            session_id,
            workspace_id: workspace.id(),
            path: PathBuf::from("entry.txt"),
        };
        (temporary, control, binding, target, root)
    }

    /// Successful native reads must not leak a result after outcome sync failure.
    #[test]
    fn read_data_is_withheld_when_outcome_sync_fails() {
        let (_temporary, mut control, binding, target, root) = fixture();
        fs::write(root.join(&target.path), b"private workspace bytes").expect("source file");
        let mut broker = FileBroker::new(&mut control);
        broker.fail_outcome_sync = true;
        assert!(matches!(
            broker.read(&binding, &target, 1024),
            Err(FileError::Audit(_))
        ));
        assert_eq!(
            fs::read(root.join(&target.path)).expect("source unchanged"),
            b"private workspace bytes"
        );
    }

    /// A completed create remains an uncertain caller outcome if audit sync fails.
    #[test]
    fn successful_creation_with_failed_audit_sync_never_returns_success() {
        let (_temporary, mut control, binding, target, root) = fixture();
        let mut broker = FileBroker::new(&mut control);
        broker.fail_outcome_sync = true;
        assert!(matches!(
            broker.create_file(&binding, &target, b"new bytes"),
            Err(FileError::Uncertain { .. })
        ));
        assert_eq!(
            fs::read(root.join(&target.path)).expect("created entry preserved"),
            b"new bytes"
        );
        // A complete outcome may have reached the log despite the sync error.
        // Reopen validates it but never replays the creation.
        assert!(
            broker
                .audit(100)
                .expect("inspect visible log")
                .entries
                .len()
                >= 3
        );
        assert!(matches!(
            broker.create_file(&binding, &target, b"retry"),
            Err(FileError::Conflict)
        ));
        assert_eq!(
            fs::read(root.join(&target.path)).expect("no retry overwrite"),
            b"new bytes"
        );
    }

    /// Preserve a partial entry and require explicit inspection acknowledgement.
    #[test]
    fn uncertain_native_creation_blocks_new_operations_until_acknowledged() {
        let (_temporary, mut control, binding, target, root) = fixture();
        let (data, store_id) = control.file_audit_context().expect("private audit context");
        let mut audit = FileAudit::open(&data, store_id).expect("audit log");
        let operation_id = audit
            .start(
                target.session_id,
                target.workspace_id,
                control.revision(),
                FileOperation::CreateFile,
                NativePath::from_path(Path::new("entry.txt")).expect("native metadata"),
            )
            .expect("durable intent");
        fs::write(root.join(&target.path), b"partial").expect("simulate partial new entry");
        let error = broker::CreateError {
            error: io::Error::other("injected post-create failure"),
            may_have_created: true,
        };
        assert!(matches!(
            complete_creation(&mut audit, operation_id, Err(error)),
            Err(FileError::Uncertain { .. })
        ));
        drop(audit);
        let mut broker = FileBroker::new(&mut control);
        assert!(
            matches!(broker.read(&binding, &target, 1024), Err(FileError::Pending { operation_id: pending }) if pending == operation_id)
        );
        assert_eq!(
            broker.audit(100).expect("inspect pending").pending,
            Some(operation_id)
        );
        broker
            .acknowledge(operation_id)
            .expect("trusted acknowledgement");
        assert_eq!(
            fs::read(root.join(&target.path)).expect("ack does not repair"),
            b"partial"
        );
        assert_eq!(
            broker
                .read(&binding, &target, 1024)
                .expect("read after inspection")
                .bytes,
            b"partial"
        );
    }
}
