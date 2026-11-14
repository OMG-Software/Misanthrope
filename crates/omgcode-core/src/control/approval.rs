//! Trusted per-call approval records.
//!
//! An approval binds a future model-tool call to a session, workspace,
//! operation, exact request digest, and the settings revision in force when it
//! was requested. It stores no argument text. It is single-use and expires.
//! No model tool consumes one yet; the core consume method is the gate a later
//! tool must call. See `docs/agent-instructions.md` and ADR-0039.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Action, ControlError, ControlSnapshot, ControlStore, valid_uuid};
use crate::policy::WorkspaceId;

/// Most approval records retained in one journal.
pub const MAX_APPROVALS: usize = 256;
/// Most undecided approvals at once.
pub const MAX_PENDING_APPROVALS: usize = 64;
/// Shortest accepted approval lifetime.
pub const MIN_APPROVAL_TTL_SECS: u64 = 60;
/// Longest accepted approval lifetime.
pub const MAX_APPROVAL_TTL_SECS: u64 = 60 * 60;
/// Longest approval summary, in bytes.
pub const MAX_APPROVAL_SUMMARY_BYTES: usize = 256;

/// The model-tool operation an approval covers.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOperation {
    /// Start a confined command.
    Command,
    /// Replace or create a workspace file.
    FileWrite,
    /// Perform a Git operation with remote access.
    GitPush,
    /// Use a destination-scoped network operation.
    Network,
}

impl ApprovalOperation {
    /// The stored spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::FileWrite => "file_write",
            Self::GitPush => "git_push",
            Self::Network => "network",
        }
    }

    /// Parse a stored or command-line spelling, case-insensitively.
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "command" => Some(Self::Command),
            "file_write" | "file-write" => Some(Self::FileWrite),
            "git_push" | "git-push" => Some(Self::GitPush),
            "network" => Some(Self::Network),
            _ => None,
        }
    }
}

/// The operator's decision.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approved,
    Denied,
}

/// The current state of one approval, computed with the current time.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    Pending,
    Approved,
    Denied,
    Consumed,
    Expired,
}

/// A requested approval, before a decision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequest {
    id: Uuid,
    session_id: Uuid,
    #[serde(deserialize_with = "super::deserialize_workspace_id")]
    workspace_id: WorkspaceId,
    operation: ApprovalOperation,
    request_sha256: String,
    summary: String,
    settings_revision: u64,
    created_unix: u64,
    expires_unix: u64,
}

/// An internal approval record: the request plus its decision and use.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(super) struct ApprovalRecord {
    request: ApprovalRequest,
    decision: Option<ApprovalDecision>,
    consumed: bool,
}

/// An operator-facing approval view.
#[derive(Clone, Debug, Serialize)]
pub struct ApprovalStatus {
    pub id: Uuid,
    pub session_id: Uuid,
    pub workspace_id: WorkspaceId,
    pub operation: ApprovalOperation,
    pub request_sha256: String,
    pub summary: String,
    pub settings_revision: u64,
    pub created_unix: u64,
    pub expires_unix: u64,
    pub state: ApprovalState,
}

/// An operator's approval request, before validation.
#[derive(Clone, Debug)]
pub struct ApprovalDeclaration {
    pub session_id: Uuid,
    pub workspace_id: WorkspaceId,
    pub operation: ApprovalOperation,
    pub request_sha256: String,
    pub summary: String,
    pub ttl_secs: u64,
}

/// The exact request a consumer must match.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalBinding {
    pub session_id: Uuid,
    pub workspace_id: WorkspaceId,
    pub operation: ApprovalOperation,
    pub request_sha256: String,
}

/// The current Unix time in seconds.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl ControlStore {
    /// Record a pending approval. This preserves the settings revision.
    pub fn request_approval(
        &mut self,
        declaration: &ApprovalDeclaration,
        now_unix: u64,
    ) -> Result<ApprovalStatus, ControlError> {
        if !valid_uuid(declaration.session_id) {
            return Err(ControlError::ApprovalRefused(
                "the session identifier must be a version 4 UUID",
            ));
        }
        if self.workspace(declaration.workspace_id).is_none() {
            return Err(ControlError::UnknownWorkspace);
        }
        if !valid_digest(&declaration.request_sha256) {
            return Err(ControlError::ApprovalRefused(
                "the request digest must be 64 lowercase hexadecimal characters",
            ));
        }
        if declaration.summary.is_empty()
            || declaration.summary.len() > MAX_APPROVAL_SUMMARY_BYTES
            || declaration.summary.contains('\0')
            || declaration.summary.chars().any(char::is_control)
        {
            return Err(ControlError::ApprovalRefused(
                "the summary must be 1 to 256 bytes without NUL or control characters",
            ));
        }
        if !(MIN_APPROVAL_TTL_SECS..=MAX_APPROVAL_TTL_SECS).contains(&declaration.ttl_secs) {
            return Err(ControlError::ApprovalRefused(
                "the approval lifetime must be 60 to 3600 seconds",
            ));
        }
        if self.snapshot.approvals.len() >= MAX_APPROVALS {
            return Err(ControlError::ApprovalRefused(
                "too many approval records are retained",
            ));
        }
        if self
            .snapshot
            .approvals
            .iter()
            .filter(|record| record.decision.is_none())
            .count()
            >= MAX_PENDING_APPROVALS
        {
            return Err(ControlError::ApprovalRefused(
                "too many undecided approvals are pending",
            ));
        }
        let request = ApprovalRequest {
            id: Uuid::new_v4(),
            session_id: declaration.session_id,
            workspace_id: declaration.workspace_id,
            operation: declaration.operation,
            request_sha256: declaration.request_sha256.clone(),
            summary: declaration.summary.clone(),
            settings_revision: self.revision(),
            created_unix: now_unix,
            expires_unix: now_unix.saturating_add(declaration.ttl_secs),
        };
        validate_request(&request)?;
        self.append(
            Action::ApprovalRequested {
                request: request.clone(),
            },
            self.revision(),
        )?;
        Ok(status_for(
            &ApprovalRecord {
                request,
                decision: None,
                consumed: false,
            },
            now_unix,
        ))
    }

    /// Approve or deny one pending approval. This preserves the settings
    /// revision.
    pub fn decide_approval(
        &mut self,
        approval_id: Uuid,
        approved: bool,
        now_unix: u64,
    ) -> Result<(), ControlError> {
        let record = self
            .approval_record(approval_id)
            .ok_or(ControlError::UnknownApproval)?;
        if record.decision.is_some() {
            return Err(ControlError::ApprovalRefused(
                "this approval already has a decision",
            ));
        }
        if record.request.expires_unix <= now_unix {
            return Err(ControlError::ApprovalRefused("this approval has expired"));
        }
        self.append(
            Action::ApprovalDecided {
                approval_id,
                decision: if approved {
                    ApprovalDecision::Approved
                } else {
                    ApprovalDecision::Denied
                },
            },
            self.revision(),
        )
    }

    /// Consume an approved request once, after checking its exact binding and
    /// that the settings revision has not changed. A future model tool must
    /// call this before the operation. This preserves the settings revision.
    pub fn consume_approval(
        &mut self,
        approval_id: Uuid,
        binding: &ApprovalBinding,
        now_unix: u64,
    ) -> Result<(), ControlError> {
        let record = self
            .approval_record(approval_id)
            .ok_or(ControlError::UnknownApproval)?;
        if record.decision != Some(ApprovalDecision::Approved) {
            return Err(ControlError::ApprovalRefused(
                "the approval was not granted",
            ));
        }
        if record.consumed {
            return Err(ControlError::ApprovalRefused(
                "the approval was already consumed",
            ));
        }
        if record.request.expires_unix <= now_unix {
            return Err(ControlError::ApprovalRefused("this approval has expired"));
        }
        if record.request.session_id != binding.session_id
            || record.request.workspace_id != binding.workspace_id
            || record.request.operation != binding.operation
            || record.request.request_sha256 != binding.request_sha256
        {
            return Err(ControlError::ApprovalRefused(
                "the request does not match the approval",
            ));
        }
        if record.request.settings_revision != self.revision() {
            return Err(ControlError::ApprovalRefused(
                "the settings changed after the approval was requested",
            ));
        }
        self.append(Action::ApprovalConsumed { approval_id }, self.revision())
    }

    /// The status of one approval, or `None` when it is unknown.
    pub fn approval_status(&self, approval_id: Uuid, now_unix: u64) -> Option<ApprovalStatus> {
        self.approval_record(approval_id)
            .map(|record| status_for(record, now_unix))
    }

    /// Every retained approval, oldest first, with its current state.
    pub fn approval_statuses(&self, now_unix: u64) -> Vec<ApprovalStatus> {
        self.snapshot
            .approvals
            .iter()
            .map(|record| status_for(record, now_unix))
            .collect()
    }

    fn approval_record(&self, approval_id: Uuid) -> Option<&ApprovalRecord> {
        self.snapshot
            .approvals
            .iter()
            .find(|record| record.request.id == approval_id)
    }
}

/// Validate and apply one approval action during replay or append preflight.
pub(super) fn apply(
    state: &mut ControlSnapshot,
    action: &Action,
    _revision: u64,
) -> Result<(), ControlError> {
    match action {
        Action::ApprovalRequested { request } => {
            validate_request(request)
                .map_err(|_| ControlError::Invalid("invalid approval request"))?;
            if state.approvals.len() >= MAX_APPROVALS
                || state
                    .approvals
                    .iter()
                    .any(|record| record.request.id == request.id)
                || !state
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.id == request.workspace_id)
                || state
                    .approvals
                    .iter()
                    .filter(|record| record.decision.is_none())
                    .count()
                    >= MAX_PENDING_APPROVALS
            {
                return Err(ControlError::Invalid(
                    "invalid, duplicate, or excessive approval request",
                ));
            }
            state.approvals.push(ApprovalRecord {
                request: request.clone(),
                decision: None,
                consumed: false,
            });
        }
        Action::ApprovalDecided {
            approval_id,
            decision,
        } => {
            let record = state
                .approvals
                .iter_mut()
                .find(|record| record.request.id == *approval_id)
                .ok_or(ControlError::Invalid("decision for an unknown approval"))?;
            if record.decision.is_some() {
                return Err(ControlError::Invalid("duplicate approval decision"));
            }
            record.decision = Some(*decision);
        }
        Action::ApprovalConsumed { approval_id } => {
            let record = state
                .approvals
                .iter_mut()
                .find(|record| record.request.id == *approval_id)
                .ok_or(ControlError::Invalid("consumption of an unknown approval"))?;
            if record.decision != Some(ApprovalDecision::Approved) || record.consumed {
                return Err(ControlError::Invalid(
                    "consumption requires an unused approved record",
                ));
            }
            record.consumed = true;
        }
        _ => {}
    }
    Ok(())
}

fn status_for(record: &ApprovalRecord, now_unix: u64) -> ApprovalStatus {
    let state = if record.consumed {
        ApprovalState::Consumed
    } else {
        match record.decision {
            Some(ApprovalDecision::Denied) => ApprovalState::Denied,
            Some(ApprovalDecision::Approved) if record.request.expires_unix <= now_unix => {
                ApprovalState::Expired
            }
            Some(ApprovalDecision::Approved) => ApprovalState::Approved,
            None if record.request.expires_unix <= now_unix => ApprovalState::Expired,
            None => ApprovalState::Pending,
        }
    };
    ApprovalStatus {
        id: record.request.id,
        session_id: record.request.session_id,
        workspace_id: record.request.workspace_id,
        operation: record.request.operation,
        request_sha256: record.request.request_sha256.clone(),
        summary: record.request.summary.clone(),
        settings_revision: record.request.settings_revision,
        created_unix: record.request.created_unix,
        expires_unix: record.request.expires_unix,
        state,
    }
}

fn validate_request(request: &ApprovalRequest) -> Result<(), ControlError> {
    if !valid_uuid(request.id)
        || !valid_uuid(request.session_id)
        || !valid_digest(&request.request_sha256)
        || request.summary.is_empty()
        || request.summary.len() > MAX_APPROVAL_SUMMARY_BYTES
        || request.summary.contains('\0')
        || request.summary.chars().any(char::is_control)
        || request.expires_unix <= request.created_unix
        || request.expires_unix - request.created_unix > MAX_APPROVAL_TTL_SECS
    {
        return Err(ControlError::ApprovalRefused(
            "the approval request is invalid",
        ));
    }
    Ok(())
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests;
