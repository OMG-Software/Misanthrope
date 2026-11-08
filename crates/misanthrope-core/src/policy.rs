//! Typed, configuration-only permission policy for workspace operations.
//!
//! A positive decision means only that configured policy permits an operation.
//! It never authorizes execution or replaces runtime checks at an executor.

use std::fmt;
use std::str::FromStr;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::{Uuid, Version};

const SESSION_BINDING_LIFETIME: Duration = Duration::from_secs(15 * 60);

/// A non-nil version 4 identifier for a workspace.
///
/// The identifier is metadata. It does not establish filesystem identity or access.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct WorkspaceId(Uuid);

impl WorkspaceId {
    /// Return the UUID value represented by this workspace identifier.
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }

    /// Validate a UUID read from trusted storage before using it as a workspace ID.
    pub(crate) fn from_uuid(value: Uuid) -> Result<Self, WorkspaceIdError> {
        if is_valid_workspace_uuid(value) {
            Ok(Self(value))
        } else {
            Err(WorkspaceIdError)
        }
    }

    /// Generate a fresh, non-nil version 4 workspace identifier.
    pub(crate) fn fresh() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for WorkspaceId {
    type Err = WorkspaceIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let uuid = Uuid::parse_str(value).map_err(|_| WorkspaceIdError)?;
        Self::from_uuid(uuid)
    }
}

/// The supplied workspace ID is malformed, nil, or is not a version 4 UUID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceIdError;

impl fmt::Display for WorkspaceIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("workspace ID must be a non-nil version 4 UUID")
    }
}

impl std::error::Error for WorkspaceIdError {}

// `Version::Random` is the UUID crate marker for version 4 UUIDs.
fn is_valid_workspace_uuid(value: Uuid) -> bool {
    !value.is_nil() && value.get_version() == Some(Version::Random)
}

/// User-configured permission flags for the four supported operation classes.
///
/// These flags are policy inputs only. They do not mint execution grants.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionSet {
    /// Whether policy permits reading workspace data.
    pub read: bool,
    /// Whether policy permits writing workspace data.
    pub write: bool,
    /// Whether policy permits process execution.
    pub execute: bool,
    /// Whether policy permits task network access.
    pub task_network: bool,
}

impl Default for PermissionSet {
    fn default() -> Self {
        Self {
            read: true,
            write: true,
            execute: false,
            task_network: false,
        }
    }
}

impl PermissionSet {
    /// Return whether the configured flag for `operation` is enabled.
    pub const fn allows(self, operation: Operation) -> bool {
        match operation {
            Operation::Read => self.read,
            Operation::Write => self.write,
            Operation::Execute => self.execute,
            Operation::TaskNetwork => self.task_network,
        }
    }
}

/// A workspace operation class used by policy evaluation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    /// Read workspace data.
    Read,
    /// Change workspace data.
    Write,
    /// Start a process.
    Execute,
    /// Allow a task to use the network.
    TaskNetwork,
}

/// An opaque, short-lived binding between a session and its workspace policy.
///
/// The binding is process-local state. It cannot be deserialized from a journal
/// or cloned to extend its lifetime. Its expiry uses a monotonic clock.
#[derive(Debug, Eq, PartialEq)]
pub struct SessionBinding {
    store_id: Uuid,
    session_id: Uuid,
    workspace_id: WorkspaceId,
    revision: u64,
    expires: Instant,
}

impl SessionBinding {
    /// Bind a session to a store, workspace, and policy revision for 15 minutes.
    pub(crate) fn new(
        store_id: Uuid,
        session_id: Uuid,
        workspace_id: WorkspaceId,
        revision: u64,
    ) -> Self {
        Self {
            store_id,
            session_id,
            workspace_id,
            revision,
            expires: Instant::now() + SESSION_BINDING_LIFETIME,
        }
    }

    /// Return the session ID bound to this policy context.
    pub const fn session_id(&self) -> Uuid {
        self.session_id
    }

    /// Return the workspace ID bound to this policy context.
    pub const fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    /// Return the policy revision bound to this session.
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

/// A request to check one operation for a session and workspace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PolicyRequest {
    /// The session that requests the operation.
    pub session_id: Uuid,
    /// The workspace that the operation targets.
    pub workspace_id: WorkspaceId,
    /// The operation class to check.
    pub operation: Operation,
}

/// Why policy evaluation allowed or rejected a request.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionReason {
    /// The configured flag is enabled for a valid binding.
    Enabled,
    /// The configured flag for the operation is disabled.
    PermissionDisabled,
    /// The binding belongs to a different policy store.
    StoreMismatch,
    /// The request belongs to a different session.
    SessionMismatch,
    /// The request targets a different workspace.
    WorkspaceMismatch,
    /// The binding refers to an older policy revision.
    StaleRevision,
    /// The binding's monotonic lifetime has ended.
    Expired,
    /// The workspace has no configured permission set.
    WorkspaceUnavailable,
}

/// A serializable description of policy eligibility, never an execution grant.
///
/// `configured_allowed` describes only stored policy. `execution_allowed` is
/// always false because an executor must perform its own authorization checks.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct PolicyDecision {
    operation: Operation,
    configured_allowed: bool,
    reason: DecisionReason,
    execution_allowed: bool,
}

impl PolicyDecision {
    /// Return the operation that was evaluated.
    pub const fn operation(self) -> Operation {
        self.operation
    }

    /// Return whether the stored policy flag permits the operation.
    pub const fn configured_allowed(self) -> bool {
        self.configured_allowed
    }

    /// Return the reason for the policy result.
    pub const fn reason(self) -> DecisionReason {
        self.reason
    }

    /// Return whether this decision grants execution. This is always false.
    pub const fn execution_allowed(self) -> bool {
        self.execution_allowed
    }
}

/// Evaluate configured policy against a process-local session binding.
///
/// Checks binding scope, revision, and expiry in a fixed order before reading
/// the workspace permission set. A missing set fails closed. A positive result
/// reports configuration eligibility only and never authorizes execution.
pub(crate) fn evaluate(
    store_id: Uuid,
    revision: u64,
    permissions: Option<PermissionSet>,
    binding: &SessionBinding,
    request: &PolicyRequest,
) -> PolicyDecision {
    let reason = if binding.store_id != store_id {
        DecisionReason::StoreMismatch
    } else if binding.session_id != request.session_id {
        DecisionReason::SessionMismatch
    } else if binding.workspace_id != request.workspace_id {
        DecisionReason::WorkspaceMismatch
    } else if binding.revision != revision {
        DecisionReason::StaleRevision
    } else if Instant::now() >= binding.expires {
        DecisionReason::Expired
    } else if permissions.is_none() {
        DecisionReason::WorkspaceUnavailable
    } else if permissions.is_some_and(|set| set.allows(request.operation)) {
        DecisionReason::Enabled
    } else {
        DecisionReason::PermissionDisabled
    };

    PolicyDecision {
        operation: request.operation,
        configured_allowed: reason == DecisionReason::Enabled,
        reason,
        execution_allowed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn id() -> Uuid {
        Uuid::new_v4()
    }

    fn workspace_id() -> WorkspaceId {
        WorkspaceId::fresh()
    }

    fn binding(
        store_id: Uuid,
        session_id: Uuid,
        workspace_id: WorkspaceId,
        revision: u64,
    ) -> SessionBinding {
        SessionBinding::new(store_id, session_id, workspace_id, revision)
    }

    fn request(session_id: Uuid, workspace_id: WorkspaceId, operation: Operation) -> PolicyRequest {
        PolicyRequest {
            session_id,
            workspace_id,
            operation,
        }
    }

    fn decision(
        binding: &SessionBinding,
        store_id: Uuid,
        revision: u64,
        permissions: Option<PermissionSet>,
        request: &PolicyRequest,
    ) -> PolicyDecision {
        evaluate(store_id, revision, permissions, binding, request)
    }

    #[test]
    fn default_permissions_enable_only_read_and_write() {
        let permissions = PermissionSet::default();

        assert!(permissions.read);
        assert!(permissions.write);
        assert!(!permissions.execute);
        assert!(!permissions.task_network);
        assert!(permissions.allows(Operation::Read));
        assert!(permissions.allows(Operation::Write));
        assert!(!permissions.allows(Operation::Execute));
        assert!(!permissions.allows(Operation::TaskNetwork));
    }

    #[test]
    fn each_operation_flag_controls_only_its_operation() {
        let cases = [
            (
                Operation::Read,
                false,
                PermissionSet {
                    read: false,
                    write: true,
                    execute: false,
                    task_network: false,
                },
            ),
            (
                Operation::Write,
                false,
                PermissionSet {
                    read: true,
                    write: false,
                    execute: false,
                    task_network: false,
                },
            ),
            (
                Operation::Execute,
                true,
                PermissionSet {
                    read: true,
                    write: true,
                    execute: true,
                    task_network: false,
                },
            ),
            (
                Operation::TaskNetwork,
                true,
                PermissionSet {
                    read: true,
                    write: true,
                    execute: false,
                    task_network: true,
                },
            ),
        ];

        for (operation, enabled, permissions) in cases {
            assert_eq!(permissions.allows(operation), enabled);
            for other in [
                Operation::Read,
                Operation::Write,
                Operation::Execute,
                Operation::TaskNetwork,
            ] {
                if other != operation {
                    assert_eq!(
                        PermissionSet::default().allows(other),
                        permissions.allows(other)
                    );
                }
            }
        }
    }

    #[test]
    fn evaluate_reports_enabled_policy_without_granting_execution() {
        let store_id = id();
        let session_id = id();
        let workspace_id = workspace_id();
        let binding = binding(store_id, session_id, workspace_id, 7);

        for operation in [Operation::Read, Operation::Write] {
            let result = decision(
                &binding,
                store_id,
                7,
                Some(PermissionSet::default()),
                &request(session_id, workspace_id, operation),
            );
            assert!(result.configured_allowed());
            assert_eq!(result.reason(), DecisionReason::Enabled);
            assert!(!result.execution_allowed());
        }
    }

    #[test]
    fn evaluate_rejects_each_disabled_operation_flag() {
        let store_id = id();
        let session_id = id();
        let workspace_id = workspace_id();
        let binding = binding(store_id, session_id, workspace_id, 2);
        let permissions = PermissionSet::default();

        for operation in [Operation::Execute, Operation::TaskNetwork] {
            let result = decision(
                &binding,
                store_id,
                2,
                Some(permissions),
                &request(session_id, workspace_id, operation),
            );
            assert!(!result.configured_allowed());
            assert_eq!(result.reason(), DecisionReason::PermissionDisabled);
            assert!(!result.execution_allowed());
        }
    }

    #[test]
    fn evaluate_rejects_a_different_store() {
        let store_id = id();
        let session_id = id();
        let workspace_id = workspace_id();
        let binding = binding(store_id, session_id, workspace_id, 1);
        let result = decision(
            &binding,
            id(),
            1,
            Some(PermissionSet::default()),
            &request(session_id, workspace_id, Operation::Read),
        );

        assert_eq!(result.reason(), DecisionReason::StoreMismatch);
        assert!(!result.configured_allowed());
        assert!(!result.execution_allowed());
    }

    #[test]
    fn evaluate_rejects_a_different_session() {
        let store_id = id();
        let workspace_id = workspace_id();
        let binding = binding(store_id, id(), workspace_id, 1);
        let result = decision(
            &binding,
            store_id,
            1,
            Some(PermissionSet::default()),
            &request(id(), workspace_id, Operation::Read),
        );

        assert_eq!(result.reason(), DecisionReason::SessionMismatch);
        assert!(!result.configured_allowed());
        assert!(!result.execution_allowed());
    }

    #[test]
    fn evaluate_rejects_a_different_workspace() {
        let store_id = id();
        let session_id = id();
        let binding = binding(store_id, session_id, workspace_id(), 1);
        let result = decision(
            &binding,
            store_id,
            1,
            Some(PermissionSet::default()),
            &request(session_id, workspace_id(), Operation::Read),
        );

        assert_eq!(result.reason(), DecisionReason::WorkspaceMismatch);
        assert!(!result.configured_allowed());
        assert!(!result.execution_allowed());
    }

    #[test]
    fn evaluate_rejects_a_stale_revision() {
        let store_id = id();
        let session_id = id();
        let workspace_id = workspace_id();
        let binding = binding(store_id, session_id, workspace_id, 3);
        let result = decision(
            &binding,
            store_id,
            4,
            Some(PermissionSet::default()),
            &request(session_id, workspace_id, Operation::Read),
        );

        assert_eq!(result.reason(), DecisionReason::StaleRevision);
        assert!(!result.configured_allowed());
        assert!(!result.execution_allowed());
    }

    #[test]
    fn evaluate_rejects_an_expired_binding() {
        let store_id = id();
        let session_id = id();
        let workspace_id = workspace_id();
        let binding = SessionBinding {
            store_id,
            session_id,
            workspace_id,
            revision: 5,
            expires: Instant::now(),
        };
        let result = decision(
            &binding,
            store_id,
            5,
            Some(PermissionSet::default()),
            &request(session_id, workspace_id, Operation::Read),
        );

        assert_eq!(result.reason(), DecisionReason::Expired);
        assert!(!result.configured_allowed());
        assert!(!result.execution_allowed());
    }

    #[test]
    fn evaluate_rejects_an_unavailable_workspace() {
        let store_id = id();
        let session_id = id();
        let workspace_id = workspace_id();
        let binding = binding(store_id, session_id, workspace_id, 9);
        let result = decision(
            &binding,
            store_id,
            9,
            None,
            &request(session_id, workspace_id, Operation::Read),
        );

        assert_eq!(result.reason(), DecisionReason::WorkspaceUnavailable);
        assert!(!result.configured_allowed());
        assert!(!result.execution_allowed());
    }

    #[test]
    fn workspace_id_accepts_only_non_nil_v4_uuids() {
        let valid = Uuid::new_v4();
        let parsed = valid
            .to_string()
            .parse::<WorkspaceId>()
            .expect("v4 ID is valid");
        assert_eq!(parsed.as_uuid(), &valid);
        assert_eq!(parsed.to_string(), valid.to_string());

        assert!(
            "00000000-0000-0000-0000-000000000000"
                .parse::<WorkspaceId>()
                .is_err()
        );
        assert!(
            "00000000-0000-1000-8000-000000000000"
                .parse::<WorkspaceId>()
                .is_err()
        );
        assert!("not-a-uuid".parse::<WorkspaceId>().is_err());
        assert!(WorkspaceId::from_uuid(Uuid::nil()).is_err());
        assert!(WorkspaceId::from_uuid(valid).is_ok());
    }

    #[test]
    fn permission_set_rejects_unknown_flags_in_json() {
        let parsed = serde_json::from_value::<PermissionSet>(json!({
            "read": true,
            "write": true,
            "execute": false,
            "task_network": false,
            "admin": true
        }));

        assert!(parsed.is_err());
    }

    #[test]
    fn policy_decision_serializes_as_description_only() {
        let store_id = id();
        let session_id = id();
        let workspace_id = workspace_id();
        let binding = binding(store_id, session_id, workspace_id, 4);
        let result = decision(
            &binding,
            store_id,
            4,
            Some(PermissionSet::default()),
            &request(session_id, workspace_id, Operation::Read),
        );
        let value = serde_json::to_value(result).expect("decision serializes");

        assert_eq!(value["execution_allowed"], false);
        assert_eq!(value["configured_allowed"], true);
    }
}
