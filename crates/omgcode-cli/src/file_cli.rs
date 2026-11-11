//! Trusted operator interface for the native file broker. Never a model tool route.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use omgcode_core::control::ControlStore;
use omgcode_core::edit::{self, EditError};
use omgcode_core::files::{FileBroker, FileError, FileTarget};
use omgcode_core::policy::WorkspaceId;
#[cfg(any(target_os = "linux", windows))]
use omgcode_core::replacement::RecordedArtifact;
use serde_json::json;
use uuid::Uuid;

use crate::{CliError, print_json};

#[derive(Args)]
pub(crate) struct Scope {
    /// Existing registered workspace ID selected by the operator.
    workspace: WorkspaceId,
    /// Trusted session selection; journal metadata never supplies a binding.
    #[arg(long)]
    session: Uuid,
}

#[derive(Subcommand)]
pub(crate) enum FileCommand {
    /// Read one bounded regular file as untrusted bytes and a content digest.
    Read {
        #[command(flatten)]
        scope: Scope,
        path: PathBuf,
        #[arg(long, default_value_t = 65536)]
        max_bytes: usize,
    },
    /// List native entry names. Omit the path to list the workspace itself.
    List {
        #[command(flatten)]
        scope: Scope,
        path: Option<PathBuf>,
        #[arg(long, default_value_t = 128)]
        limit: usize,
    },
    /// Exclusively create a new UTF-8 text file; existing entries are refused.
    Create {
        #[command(flatten)]
        scope: Scope,
        path: PathBuf,
        #[arg(long)]
        content: String,
    },
    /// Exclusively create one directory beneath an existing parent.
    Mkdir {
        #[command(flatten)]
        scope: Scope,
        path: PathBuf,
    },
    /// Preview a bounded diff against trusted proposed content without writing.
    Diff {
        #[command(flatten)]
        scope: Scope,
        /// Existing workspace-relative file to compare.
        path: PathBuf,
        /// Trusted local file containing the complete proposed UTF-8 content.
        #[arg(long)]
        content_file: PathBuf,
        /// Maximum accepted size of either side of the comparison.
        #[arg(long, default_value_t = edit::MAX_EDIT_CONTENT_BYTES)]
        max_bytes: usize,
    },
    /// Replace one existing file after checkpointing its exact prior bytes.
    ///
    /// Stop every other writer before this operation. The expected digest and
    /// native replacement do not provide atomic compare-and-swap and cannot
    /// prevent concurrent edits or guarantee that every such edit is saved.
    Apply {
        #[command(flatten)]
        scope: Scope,
        /// Existing workspace-relative regular file to replace.
        path: PathBuf,
        /// Trusted local file containing the complete proposed UTF-8 content.
        #[arg(long)]
        content_file: PathBuf,
        /// Expected SHA-256 of the current file's exact bytes.
        #[arg(long)]
        expected_sha256: String,
        /// Expected byte count of the current file.
        #[arg(long)]
        expected_bytes: u64,
        /// Acknowledge that all other writers are stopped. This does not provide CAS.
        #[arg(long, required = true, action = clap::ArgAction::SetTrue)]
        writers_stopped: bool,
    },
    /// Restore a retained checkpoint's original bytes over the current file.
    ///
    /// The expected digest and size describe the current file, never the
    /// checkpoint. The current bytes are checkpointed again before publication.
    /// Stop every other writer; this does not provide atomic compare-and-swap.
    Restore {
        #[command(flatten)]
        scope: Scope,
        /// Existing workspace-relative regular file that the checkpoint recorded.
        path: PathBuf,
        /// Retained checkpoint operation whose original bytes are restored.
        #[arg(long)]
        checkpoint: Uuid,
        /// Expected SHA-256 of the current file's exact bytes.
        #[arg(long)]
        expected_sha256: String,
        /// Expected byte count of the current file.
        #[arg(long)]
        expected_bytes: u64,
        /// Acknowledge that all other writers are stopped. This does not provide CAS.
        #[arg(long, required = true, action = clap::ArgAction::SetTrue)]
        writers_stopped: bool,
    },
    /// Inspect validated private file-operation metadata and pending recovery.
    Audit {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Inspect an existing archived log rather than the active journal.
        #[arg(long)]
        archive: Option<PathBuf>,
    },
    /// Archive a completed log without replacing, pruning, or replaying records.
    Rotate {
        /// Existing private directory outside development data, on the same filesystem.
        #[arg(long)]
        archive_dir: PathBuf,
    },
    /// Record inspection of an uncertain operation, without retrying or repairing it.
    Acknowledge { operation_id: Uuid },
    /// Inspect existing checkpoint metadata without creating storage or applying changes.
    Checkpoints,
    /// Record inspection of one checkpoint operation without applying or replaying it.
    AcknowledgeCheckpoint { operation_id: Uuid },
    /// Permanently delete one resolved checkpoint entry; its ledger history stays.
    ///
    /// Repeat the same command to finish an interrupted deletion. No workspace
    /// file is opened or changed.
    DeleteCheckpoint {
        operation_id: Uuid,
        /// Acknowledge that the deleted bytes can no longer be restored.
        #[arg(long, required = true, action = clap::ArgAction::SetTrue)]
        permanent: bool,
    },
}

/// Restore output: the new operation's receipt and the checkpoint it read.
#[cfg(any(target_os = "linux", windows))]
#[derive(serde::Serialize)]
struct RestoreOutput {
    restored_from: Uuid,
    #[serde(flatten)]
    receipt: omgcode_core::replacement::ReplacementReceipt,
}

/// Open existing trusted settings. No command silently chooses a development root.
pub(crate) fn run(command: FileCommand, data_root: &Path) -> Result<(), CliError> {
    let mut control = ControlStore::open_existing(data_root)
        .map_err(|error| CliError::new("control", error.to_string()))?
        .ok_or_else(|| CliError::new("control", "Workspace settings are not initialized"))?;
    match command {
        FileCommand::Read {
            scope,
            path,
            max_bytes,
        } => {
            let binding = control
                .bind_session(scope.session, scope.workspace)
                .map_err(|error| CliError::new("control", error.to_string()))?;
            let target = target(scope, path);
            print_json(
                &FileBroker::new(&mut control)
                    .read(&binding, &target, max_bytes)
                    .map_err(file_error)?,
            )
        }
        FileCommand::List { scope, path, limit } => {
            let binding = control
                .bind_session(scope.session, scope.workspace)
                .map_err(|error| CliError::new("control", error.to_string()))?;
            let target = target(scope, path.unwrap_or_default());
            print_json(
                &FileBroker::new(&mut control)
                    .list(&binding, &target, limit)
                    .map_err(file_error)?,
            )
        }
        FileCommand::Create {
            scope,
            path,
            content,
        } => {
            let binding = control
                .bind_session(scope.session, scope.workspace)
                .map_err(|error| CliError::new("control", error.to_string()))?;
            let target = target(scope, path);
            print_json(
                &FileBroker::new(&mut control)
                    .create_file(&binding, &target, content.as_bytes())
                    .map_err(file_error)?,
            )
        }
        FileCommand::Mkdir { scope, path } => {
            let binding = control
                .bind_session(scope.session, scope.workspace)
                .map_err(|error| CliError::new("control", error.to_string()))?;
            let target = target(scope, path);
            print_json(
                &FileBroker::new(&mut control)
                    .create_directory(&binding, &target)
                    .map_err(file_error)?,
            )
        }
        FileCommand::Diff {
            scope,
            path,
            content_file,
            max_bytes,
        } => {
            if !(1..=edit::MAX_EDIT_CONTENT_BYTES).contains(&max_bytes) {
                return Err(CliError::new(
                    "edit_request",
                    "Edit-preview limit must be 1 byte through 1 MiB",
                ));
            }
            let binding = control
                .bind_session(scope.session, scope.workspace)
                .map_err(|error| CliError::new("control", error.to_string()))?;
            let target = target(scope, path);
            let current = FileBroker::new(&mut control)
                .read(&binding, &target, max_bytes)
                .map_err(file_error)?;
            let proposed =
                edit::read_proposed_file(&content_file, max_bytes).map_err(proposed_error)?;
            print_json(
                &edit::preview(&current.bytes, &proposed, current.target).map_err(edit_error)?,
            )
        }
        FileCommand::Apply {
            scope,
            path,
            content_file,
            expected_sha256,
            expected_bytes,
            writers_stopped: _,
        } => {
            #[cfg(not(any(target_os = "linux", windows)))]
            {
                let _ = (scope, path, content_file, expected_sha256, expected_bytes);
                Err(CliError::new(
                    "file_unsupported",
                    "File replacement is available only on Linux and Windows",
                ))
            }
            #[cfg(any(target_os = "linux", windows))]
            {
                let binding = control
                    .bind_session(scope.session, scope.workspace)
                    .map_err(|error| CliError::new("control", error.to_string()))?;
                let target = target(scope, path);
                let proposed =
                    edit::read_proposed_file(&content_file, edit::MAX_EDIT_CONTENT_BYTES)
                        .map_err(proposed_error)?;
                let expected = RecordedArtifact {
                    bytes: expected_bytes,
                    sha256: expected_sha256,
                };
                print_json(
                    &FileBroker::new(&mut control)
                        .apply_replacement(&binding, &target, &expected, &proposed)
                        .map_err(file_error)?,
                )
            }
        }
        FileCommand::Restore {
            scope,
            path,
            checkpoint,
            expected_sha256,
            expected_bytes,
            writers_stopped: _,
        } => {
            #[cfg(not(any(target_os = "linux", windows)))]
            {
                let _ = (scope, path, checkpoint, expected_sha256, expected_bytes);
                Err(CliError::new(
                    "file_unsupported",
                    "File restore is available only on Linux and Windows",
                ))
            }
            #[cfg(any(target_os = "linux", windows))]
            {
                let binding = control
                    .bind_session(scope.session, scope.workspace)
                    .map_err(|error| CliError::new("control", error.to_string()))?;
                let target = target(scope, path);
                let expected = RecordedArtifact {
                    bytes: expected_bytes,
                    sha256: expected_sha256,
                };
                let receipt = FileBroker::new(&mut control)
                    .restore_checkpoint(&binding, &target, checkpoint, &expected)
                    .map_err(file_error)?;
                print_json(&RestoreOutput {
                    restored_from: checkpoint,
                    receipt,
                })
            }
        }
        FileCommand::Audit { limit, archive } => {
            let mut broker = FileBroker::new(&mut control);
            let view = match archive {
                Some(path) => broker.audit_archive(&path, limit),
                None => broker.audit(limit),
            }
            .map_err(file_error)?;
            print_json(&view)
        }
        FileCommand::Rotate { archive_dir } => print_json(
            &FileBroker::new(&mut control)
                .rotate_audit(&archive_dir)
                .map_err(file_error)?,
        ),
        FileCommand::Acknowledge { operation_id } => {
            FileBroker::new(&mut control)
                .acknowledge(operation_id)
                .map_err(file_error)?;
            print_json(&json!({"acknowledged_operation": operation_id, "replayed": false}))
        }
        FileCommand::Checkpoints => print_json(
            &FileBroker::new(&mut control)
                .checkpoint_inspect()
                .map_err(file_error)?,
        ),
        FileCommand::AcknowledgeCheckpoint { operation_id } => {
            FileBroker::new(&mut control)
                .checkpoint_acknowledge(operation_id)
                .map_err(file_error)?;
            print_json(&json!({
                "acknowledged_checkpoint": operation_id,
                "replayed": false,
                "applied": false,
            }))
        }
        FileCommand::DeleteCheckpoint {
            operation_id,
            permanent: _,
        } => print_json(
            &FileBroker::new(&mut control)
                .checkpoint_delete(operation_id)
                .map_err(file_error)?,
        ),
    }
}

/// Keep scope explicit; copying these descriptive arguments never grants access.
fn target(scope: Scope, path: PathBuf) -> FileTarget {
    FileTarget {
        session_id: scope.session,
        workspace_id: scope.workspace,
        path,
    }
}

/// Return stable categories for trusted proposed-content read failures.
fn proposed_error(error: edit::ProposedReadError) -> CliError {
    let code = match error {
        edit::ProposedReadError::TooLarge => "edit_limit",
        edit::ProposedReadError::ChangedDuringRead
        | edit::ProposedReadError::NotRegular
        | edit::ProposedReadError::Unsupported
        | edit::ProposedReadError::Io => "edit_request",
    };
    CliError::new(code, error.to_string())
}

/// Return stable categories for non-destructive edit-preview failures.
fn edit_error(error: EditError) -> CliError {
    let code = match error {
        EditError::CurrentTooLarge | EditError::ProposedTooLarge | EditError::TooManyLines => {
            "edit_limit"
        }
        EditError::CurrentNotText | EditError::ProposedNotText => "edit_text",
    };
    CliError::new(code, error.to_string())
}

/// Return stable categories while retaining a useful, escaped operator message.
fn file_error(error: FileError) -> CliError {
    let code = match &error {
        FileError::Denied(_) => "file_denied",
        FileError::Conflict => "file_conflict",
        FileError::Invalid(_) | FileError::InvalidPath(_) | FileError::Encoding(_) => {
            "file_request"
        }
        FileError::Pending { .. } => "file_pending",
        FileError::Uncertain { .. } => "file_uncertain",
        FileError::RotationUncertain { .. } => "file_rotation_uncertain",
        FileError::Audit(_) => "file_audit",
        FileError::Control(_) => "control",
        FileError::Checkpoint(error) => error.code(),
        FileError::Native(error) if error.kind() == std::io::ErrorKind::Unsupported => {
            "file_unsupported"
        }
        FileError::Native(_) => "file_io",
    };
    CliError::new(code, error.to_string())
}
