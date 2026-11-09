//! Trusted operator interface for the native file broker. Never a model tool route.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use misanthrope_core::control::ControlStore;
use misanthrope_core::files::{FileBroker, FileError, FileTarget};
use misanthrope_core::policy::WorkspaceId;
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
    /// Inspect validated private file-operation metadata and pending recovery.
    Audit {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Record inspection of an uncertain operation, without retrying or repairing it.
    Acknowledge { operation_id: Uuid },
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
        FileCommand::Audit { limit } => print_json(
            &FileBroker::new(&mut control)
                .audit(limit)
                .map_err(file_error)?,
        ),
        FileCommand::Acknowledge { operation_id } => {
            FileBroker::new(&mut control)
                .acknowledge(operation_id)
                .map_err(file_error)?;
            print_json(&json!({"acknowledged_operation": operation_id, "replayed": false}))
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

/// Return stable categories while retaining a useful, escaped operator message.
fn file_error(error: FileError) -> CliError {
    let code = match &error {
        FileError::Denied(_) => "file_denied",
        FileError::Conflict => "file_conflict",
        FileError::Invalid(_) | FileError::Encoding(_) => "file_request",
        FileError::Pending { .. } => "file_pending",
        FileError::Uncertain { .. } => "file_uncertain",
        FileError::Audit(_) => "file_audit",
        FileError::Control(_) => "control",
        FileError::Native(error) if error.kind() == std::io::ErrorKind::Unsupported => {
            "file_unsupported"
        }
        FileError::Native(_) => "file_io",
    };
    CliError::new(code, error.to_string())
}
