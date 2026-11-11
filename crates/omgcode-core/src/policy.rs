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

    /// Wrap a stored UUID only after enforcing the non-nil version 4 invariant.
    pub(crate) fn from_uuid(value: Uuid) -> Result<Self, WorkspaceIdError> {
        if is_valid_workspace_uuid(value) {
            Ok(Self(value))
        } else {
            Err(WorkspaceIdError)
        }
    }

    /// Generate a fresh non-nil version 4 ID that matches parser and storage rules.
    pub(crate) fn fresh() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for WorkspaceId {
    /// Render the UUID in the standard display form for local configuration.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for WorkspaceId {
    type Err = WorkspaceIdError;

    /// Parse UUID syntax, then enforce the non-nil version 4 workspace invariant.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let uuid = Uuid::parse_str(value).map_err(|_| WorkspaceIdError)?;
        Self::from_uuid(uuid)
    }
}

/// The supplied workspace ID is malformed, nil, or is not a version 4 UUID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceIdError;

impl fmt::Display for WorkspaceIdError {
    /// Keep invalid syntax and unsupported UUID forms on one safe error message.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("workspace ID must be a non-nil version 4 UUID")
    }
}

impl std::error::Error for WorkspaceIdError {}

/// Keep parsed and generated workspace IDs within the same non-nil UUID v4 domain.
fn is_valid_workspace_uuid(value: Uuid) -> bool {
    !value.is_nil() && value.get_version() == Some(Version::Random)
}

/// User-configured permission flags for the four supported operation classes
/// and the model read/list tool preference.
///
/// These flags are policy inputs only. They do not mint execution grants.
/// `model_tools` is omitted from persisted JSON while it is `false`, so
/// settings written without it keep their exact bytes and older readers can
/// still open them. Once enabled, older readers refuse the field.
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
    /// Whether chat may offer the model the read and list workspace tools.
    ///
    /// Each model call still needs the live `read` preference and a fresh
    /// binding, and stays confined to the session workspace.
    #[serde(default, skip_serializing_if = "is_false")]
    pub model_tools: bool,
}

/// Keep a disabled optional preference out of persisted settings records.
#[allow(clippy::trivially_copy_pass_by_ref)] // serde passes the field by reference.
const fn is_false(value: &bool) -> bool {
    !*value
}

impl Default for PermissionSet {
    /// Leave process execution, task networking, and model tools disabled until configured.
    fn default() -> Self {
        Self {
            read: true,
            write: true,
            execute: false,
            task_network: false,
            model_tools: false,
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
    ///
    /// If the monotonic clock cannot represent the lifetime, the binding expires
    /// immediately.
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
            // Clock-range exhaustion expires the binding immediately.
            expires: {
                let now = Instant::now();
                now.checked_add(SESSION_BINDING_LIFETIME).unwrap_or(now)
            },
        }
    }

    /// Identify the store incarnation for internal audit records.
    pub(crate) const fn store_id(&self) -> Uuid {
        self.store_id
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

    /// Generate an independent UUID for each test scope.
    fn id() -> Uuid {
        Uuid::new_v4()
    }

    /// Use a generated non-nil version 4 ID in policy test fixtures.
    fn workspace_id() -> WorkspaceId {
        WorkspaceId::fresh()
    }

    /// Build an opaque live binding for the supplied store and session fixture.
    fn binding(
        store_id: Uuid,
        session_id: Uuid,
        workspace_id: WorkspaceId,
        revision: u64,
    ) -> SessionBinding {
        SessionBinding::new(store_id, session_id, workspace_id, revision)
    }

    /// Describe one session-scoped operation request for evaluator tests.
    fn request(session_id: Uuid, workspace_id: WorkspaceId, operation: Operation) -> PolicyRequest {
        PolicyRequest {
            session_id,
            workspace_id,
            operation,
        }
    }

    /// Apply one request to the supplied policy context and permission snapshot.
    fn decision(
        binding: &SessionBinding,
        store_id: Uuid,
        revision: u64,
        permissions: Option<PermissionSet>,
        request: &PolicyRequest,
    ) -> PolicyDecision {
        evaluate(store_id, revision, permissions, binding, request)
    }

    /// Preserve the documented defaults while keeping higher-impact options off.
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

    /// Verify that changing one permission flag does not change another operation.
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
                    model_tools: false,
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
                    model_tools: false,
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
                    model_tools: false,
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
                    model_tools: false,
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

    /// Confirm that enabled configuration policy still produces no execution grant.
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

    /// Deny execution and task-network requests under their default disabled flags.
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

    /// Bindings from an earlier or separate store incarnation must fail closed.
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

    /// A valid workspace binding cannot be reused by another session.
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

    /// Requests for a different workspace must not inherit the bound policy.
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

    /// A policy edit invalidates bindings created under the previous revision.
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

    /// A monotonic lifetime expiry blocks an otherwise matching request.
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

    /// Missing workspace preferences are an unavailable policy, not an allow.
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

    /// Keep malformed, nil, and other-version IDs outside the workspace type.
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

    /// Keep settings written without the model tool preference byte-compatible.
    #[test]
    fn model_tools_is_absent_while_disabled_and_explicit_when_enabled() {
        let legacy = json!({
            "read": true,
            "write": true,
            "execute": false,
            "task_network": false
        });
        let parsed = serde_json::from_value::<PermissionSet>(legacy.clone()).expect("legacy set");
        assert_eq!(parsed, PermissionSet::default());
        assert!(!parsed.model_tools);
        assert_eq!(serde_json::to_value(parsed).expect("encode"), legacy);

        let enabled = PermissionSet {
            model_tools: true,
            ..PermissionSet::default()
        };
        let encoded = serde_json::to_value(enabled).expect("encode enabled");
        assert_eq!(encoded["model_tools"], true);
        assert_eq!(
            serde_json::from_value::<PermissionSet>(encoded).expect("decode enabled"),
            enabled
        );
        for operation in [
            Operation::Read,
            Operation::Write,
            Operation::Execute,
            Operation::TaskNetwork,
        ] {
            assert_eq!(
                enabled.allows(operation),
                PermissionSet::default().allows(operation)
            );
        }
    }

    /// Reject future or attacker-added permission fields instead of ignoring them.
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

    /// Persist a descriptive decision without exposing a deserialization grant path.
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
