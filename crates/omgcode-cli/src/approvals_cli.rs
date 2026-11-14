//! Trusted operator approval commands.
#![forbid(unsafe_code)]

use std::path::Path;
use std::str::FromStr;
use std::thread;
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use omgcode_core::control::{
    ApprovalBinding, ApprovalDeclaration, ApprovalOperation, ControlError, ControlStore, now_unix,
};
use omgcode_core::policy::WorkspaceId;
use serde_json::json;
use uuid::Uuid;

use crate::{CliError, print_json};

/// Inspect, decide, or consume trusted per-call approvals.
#[derive(Subcommand)]
pub(crate) enum ApprovalsCommand {
    /// Record a pending approval request. Nothing executes.
    Request(RequestArgs),
    /// List retained approvals and their current states.
    List,
    /// Show one approval.
    Show { approval: Uuid },
    /// Approve a pending, unexpired request.
    Approve { approval: Uuid },
    /// Deny a pending, unexpired request.
    Deny { approval: Uuid },
    /// Consume an approved request once. This executes nothing.
    Consume(ConsumeArgs),
}

#[derive(Args)]
pub(crate) struct RequestArgs {
    /// The session the future call belongs to (version 4 UUID).
    #[arg(long)]
    session: Uuid,
    /// The registered workspace the future call targets.
    #[arg(long)]
    workspace: String,
    /// command, file_write, git_push, or network.
    #[arg(long)]
    operation: String,
    /// SHA-256 of the exact future call, as 64 lowercase hex characters.
    #[arg(long)]
    digest: String,
    /// Operator-facing context. At most 256 bytes; never include secrets.
    #[arg(long)]
    summary: String,
    /// Lifetime in seconds (60 to 3600).
    #[arg(long, default_value_t = 900)]
    ttl_secs: u64,
}

#[derive(Args)]
pub(crate) struct ConsumeArgs {
    approval: Uuid,
    /// The session the call belongs to.
    #[arg(long)]
    session: Uuid,
    /// The workspace the call targets.
    #[arg(long)]
    workspace: String,
    /// command, file_write, git_push, or network.
    #[arg(long)]
    operation: String,
    /// SHA-256 of the exact call.
    #[arg(long)]
    digest: String,
}

const CONTROL_WAIT: Duration = Duration::from_secs(10);
const CONTROL_POLL: Duration = Duration::from_millis(20);

/// Run one approval command.
pub(crate) fn run(action: ApprovalsCommand, data_root: &Path) -> Result<(), CliError> {
    match action {
        ApprovalsCommand::Request(args) => request(args, data_root),
        ApprovalsCommand::List => {
            let store = open(data_root)?;
            let now = now_unix();
            print_json(&json!({
                "now": now,
                "approvals": store.approval_statuses(now),
            }))
        }
        ApprovalsCommand::Show { approval } => {
            let store = open(data_root)?;
            let status = store
                .approval_status(approval, now_unix())
                .ok_or_else(|| approval_error(ControlError::UnknownApproval))?;
            print_json(&json!({ "approval": status }))
        }
        ApprovalsCommand::Approve { approval } => decide(approval, true, data_root),
        ApprovalsCommand::Deny { approval } => decide(approval, false, data_root),
        ApprovalsCommand::Consume(args) => consume(args, data_root),
    }
}

fn request(args: RequestArgs, data_root: &Path) -> Result<(), CliError> {
    let workspace_id = parse_workspace(&args.workspace)?;
    let operation = ApprovalOperation::parse(&args.operation).ok_or_else(|| {
        CliError::new(
            "configuration",
            "Operation must be command, file_write, git_push, or network",
        )
    })?;
    let mut store = open(data_root)?;
    let status = store
        .request_approval(
            &ApprovalDeclaration {
                session_id: args.session,
                workspace_id,
                operation,
                request_sha256: args.digest,
                summary: args.summary,
                ttl_secs: args.ttl_secs,
            },
            now_unix(),
        )
        .map_err(approval_error)?;
    print_json(&json!({ "approval": status }))
}

fn decide(approval: Uuid, approved: bool, data_root: &Path) -> Result<(), CliError> {
    let mut store = open(data_root)?;
    let now = now_unix();
    store
        .decide_approval(approval, approved, now)
        .map_err(approval_error)?;
    let status = store
        .approval_status(approval, now)
        .ok_or_else(|| approval_error(ControlError::UnknownApproval))?;
    print_json(&json!({
        "decision": if approved { "approved" } else { "denied" },
        "approval": status,
    }))
}

fn consume(args: ConsumeArgs, data_root: &Path) -> Result<(), CliError> {
    let workspace_id = parse_workspace(&args.workspace)?;
    let operation = ApprovalOperation::parse(&args.operation).ok_or_else(|| {
        CliError::new(
            "configuration",
            "Operation must be command, file_write, git_push, or network",
        )
    })?;
    let mut store = open(data_root)?;
    store
        .consume_approval(
            args.approval,
            &ApprovalBinding {
                session_id: args.session,
                workspace_id,
                operation,
                request_sha256: args.digest,
            },
            now_unix(),
        )
        .map_err(approval_error)?;
    print_json(&json!({
        "consumed": args.approval,
        "approval": store.approval_status(args.approval, now_unix()),
    }))
}

fn parse_workspace(text: &str) -> Result<WorkspaceId, CliError> {
    WorkspaceId::from_str(text)
        .map_err(|_| CliError::new("configuration", "Invalid workspace identifier"))
}

/// Open existing settings, waiting briefly for another command's lock.
fn open(data_root: &Path) -> Result<ControlStore, CliError> {
    let until = Instant::now() + CONTROL_WAIT;
    loop {
        match ControlStore::open_existing(data_root) {
            Ok(Some(store)) => return Ok(store),
            Ok(None) => {
                return Err(CliError::new(
                    "control",
                    "Workspace control is not initialized",
                ));
            }
            Err(ControlError::Busy) if Instant::now() < until => thread::sleep(CONTROL_POLL),
            Err(error) => return Err(control_error(error)),
        }
    }
}

fn control_error(error: ControlError) -> CliError {
    CliError::new("control", error.to_string())
}

fn approval_error(error: ControlError) -> CliError {
    match error {
        ControlError::ApprovalRefused(_) | ControlError::UnknownApproval => {
            CliError::new("approval", error.to_string())
        }
        other => control_error(other),
    }
}
