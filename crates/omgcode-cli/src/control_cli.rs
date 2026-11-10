//! Operator commands for workspace registration and configuration-only policy checks.
#![forbid(unsafe_code)]

use clap::{Args, Subcommand, ValueEnum};
use omgcode_core::control::{ControlError, ControlStore};
use omgcode_core::policy::{Operation, PermissionSet, PolicyRequest, WorkspaceId};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use uuid::Uuid;

use crate::{CliError, print_json};

#[derive(Subcommand)]
pub(crate) enum WorkspaceCommand {
    /// Display and initialize the current trusted workspace configuration.
    Status,
    /// Set the maximum development-data root using an expected revision.
    SetRoot {
        path: PathBuf,
        #[arg(long)]
        revision: u64,
    },
    /// Register a workspace below the configured development root.
    Register {
        path: PathBuf,
        #[arg(long)]
        revision: u64,
    },
    /// Change one or more policy flags using an expected revision.
    Permissions(PermissionsArgs),
    /// Evaluate one diagnostic policy request and record it in the audit log.
    Check(CheckArgs),
    /// Show recent policy audit entries.
    Audit {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

#[derive(Args)]
pub(crate) struct PermissionsArgs {
    id: String,
    #[arg(long)]
    revision: u64,
    #[arg(long)]
    read: Option<bool>,
    #[arg(long)]
    write: Option<bool>,
    #[arg(long)]
    execute: Option<bool>,
    #[arg(long = "task-network")]
    task_network: Option<bool>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum OperationArg {
    Read,
    Write,
    Execute,
    TaskNetwork,
}

impl From<OperationArg> for Operation {
    fn from(value: OperationArg) -> Self {
        match value {
            OperationArg::Read => Self::Read,
            OperationArg::Write => Self::Write,
            OperationArg::Execute => Self::Execute,
            OperationArg::TaskNetwork => Self::TaskNetwork,
        }
    }
}

#[derive(Args)]
pub(crate) struct CheckArgs {
    id: String,
    #[arg(long)]
    session: Uuid,
    #[arg(long, value_enum)]
    operation: OperationArg,
    #[arg(long)]
    target_workspace: Option<String>,
}

/// Run one control command against trusted application storage.
pub(crate) fn run(action: WorkspaceCommand, data_root: &Path) -> Result<(), CliError> {
    match action {
        WorkspaceCommand::Status => {
            let store = open(data_root)?;
            print_json(&store.snapshot())
        }
        WorkspaceCommand::SetRoot { path, revision } => {
            let mut store = open(data_root)?;
            store
                .set_development_root(&path, revision)
                .map_err(control_error)?;
            print_json(&store.snapshot())
        }
        WorkspaceCommand::Register { path, revision } => {
            let mut store = open(data_root)?;
            let registration = store
                .register_workspace(&path, revision)
                .map_err(control_error)?;
            print_json(&json!({
                "registration_id": registration.id(),
                "snapshot": store.snapshot(),
            }))
        }
        WorkspaceCommand::Permissions(args) => set_permissions(args, data_root),
        WorkspaceCommand::Check(args) => check(args, data_root),
        WorkspaceCommand::Audit { limit } => audit(limit, data_root),
    }
}

fn set_permissions(args: PermissionsArgs, data_root: &Path) -> Result<(), CliError> {
    if args.read.is_none()
        && args.write.is_none()
        && args.execute.is_none()
        && args.task_network.is_none()
    {
        return Err(CliError::new(
            "configuration",
            "Set at least one permission flag",
        ));
    }
    let id = parse_workspace_id(&args.id)?;
    let mut store = open(data_root)?;
    let current = store
        .workspace(id)
        .ok_or_else(|| CliError::new("control", "Workspace is not registered"))?
        .permissions();
    let permissions = PermissionSet {
        read: args.read.unwrap_or(current.read),
        write: args.write.unwrap_or(current.write),
        execute: args.execute.unwrap_or(current.execute),
        task_network: args.task_network.unwrap_or(current.task_network),
    };
    store
        .set_permissions(id, permissions, args.revision)
        .map_err(control_error)?;
    print_json(&store.snapshot())
}

fn check(args: CheckArgs, data_root: &Path) -> Result<(), CliError> {
    let id = parse_workspace_id(&args.id)?;
    let target_id = args
        .target_workspace
        .as_deref()
        .map(parse_workspace_id)
        .transpose()?
        .unwrap_or(id);
    let mut store = open_existing(data_root)?
        .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
    let binding = store
        .bind_session(args.session, id)
        .map_err(control_error)?;
    let decision = store
        .evaluate(
            &binding,
            &PolicyRequest {
                session_id: args.session,
                workspace_id: target_id,
                operation: args.operation.into(),
            },
        )
        .map_err(control_error)?;
    print_json(&json!({"decision": decision, "snapshot": store.snapshot()}))
}

fn audit(limit: usize, data_root: &Path) -> Result<(), CliError> {
    if !(1..=100).contains(&limit) {
        return Err(CliError::new(
            "configuration",
            "Audit limit must be 1 to 100",
        ));
    }
    let store = open_existing(data_root)?
        .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
    let entries = store.audit(limit).map_err(control_error)?;
    print_json(&json!({"audit": entries}))
}

fn open(data_root: &Path) -> Result<ControlStore, CliError> {
    ControlStore::open(data_root).map_err(control_error)
}

fn open_existing(data_root: &Path) -> Result<Option<ControlStore>, CliError> {
    ControlStore::open_existing(data_root).map_err(control_error)
}

fn parse_workspace_id(value: &str) -> Result<WorkspaceId, CliError> {
    WorkspaceId::from_str(value)
        .map_err(|_| CliError::new("configuration", "Invalid workspace identifier"))
}

fn control_error(error: ControlError) -> CliError {
    CliError::new("control", error.to_string())
}
