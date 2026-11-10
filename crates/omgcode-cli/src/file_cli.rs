//! Trusted operator interface for the native file broker. Never a model tool route.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use omgcode_core::control::ControlStore;
use omgcode_core::edit::{self, EditError};
use omgcode_core::files::{FileBroker, FileError, FileTarget};
use omgcode_core::policy::WorkspaceId;
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
