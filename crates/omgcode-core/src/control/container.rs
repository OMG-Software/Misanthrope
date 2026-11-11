//! Recorded, reversible AppContainer grants for workspace containers.
//!
//! Each workspace has one container identity, `OMGCode.Workspace.<id>`. A
//! grant gives that container read-execute access to a directory, or
//! read-write access to the workspace directory itself. The journal records
//! the intent before the ACL changes and the outcome after it, so a grant
//! whose outcome is unknown stays visible and revocable. Revocation is never
//! gated on a settings revision, and the journal keeps room for it. Removing
//! the last grant of a container also deletes its AppContainer profile.
//!
//! Grants are ACL entries, not a sandbox by themselves: only
//! `omgcode_platform::process::run_confined` runs a program in the container.
//! See `docs/execution.md` and ADR-0029.

use std::path::Path;

use omgcode_platform::container::{
    self as platform, CONTAINER_GRANTS_SUPPORTED, ContainerAccess, ContainerName,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    Action, ControlError, ControlSnapshot, ControlStore, deserialize_workspace_id, valid_uuid,
};
use crate::policy::WorkspaceId;
use crate::workspace::DirectoryRecord;

/// Most grants, in any state, for one workspace container.
pub(super) const MAX_GRANTS_PER_WORKSPACE: usize = 32;
/// Most grants, in any state, across every workspace.
pub(super) const MAX_GRANTS: usize = 256;
/// Budget for one small grant outcome event (`applied`, `revoke requested`,
/// or `revoked`). Real events are about 300 bytes.
pub(super) const GRANT_EVENT_RESERVE_BYTES: usize = 512;
/// Longest accepted string SID. Container SIDs are about 90 characters.
const MAX_SID_CHARS: usize = 184;
const CONTAINER_PREFIX: &str = "OMGCode.Workspace.";

/// Access recorded for one grant.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerGrantAccess {
    /// Read, list, traverse, and execute.
    ReadExecute,
    /// Also create, write, rename, and delete. Only the workspace directory.
    ReadWrite,
}

impl From<ContainerGrantAccess> for ContainerAccess {
    fn from(value: ContainerGrantAccess) -> Self {
        match value {
            ContainerGrantAccess::ReadExecute => Self::ReadExecute,
            ContainerGrantAccess::ReadWrite => Self::ReadWrite,
        }
    }
}

/// Where a grant is in its lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerGrantState {
    /// The intent is recorded; the ACL change was not confirmed. The entry
    /// may or may not exist. Revoke it to be certain.
    Pending,
    /// The entry was written and read back.
    Active,
    /// Revocation was requested and not confirmed. Retry the revocation.
    Revoking,
}

/// One persisted grant. Paths keep their native encoding and must stay private.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerGrant {
    id: Uuid,
    #[serde(deserialize_with = "deserialize_workspace_id")]
    workspace_id: WorkspaceId,
    container: String,
    sid: String,
    directory: DirectoryRecord,
    access: ContainerGrantAccess,
}

impl ContainerGrant {
    /// Stable grant identifier used for revocation.
    pub const fn id(&self) -> Uuid {
        self.id
    }

    /// The workspace whose container holds this grant.
    pub const fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    /// The recorded access level.
    pub const fn access(&self) -> ContainerGrantAccess {
        self.access
    }

    /// The granted directory as recorded.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) const fn directory(&self) -> &DirectoryRecord {
        &self.directory
    }
}

/// A grant and its current state, as reported in status output.
#[derive(Clone, Debug, Serialize)]
pub struct ContainerGrantStatus {
    #[serde(flatten)]
    grant: ContainerGrant,
    state: ContainerGrantState,
}

impl ContainerGrantStatus {
    /// The recorded grant.
    pub const fn grant(&self) -> &ContainerGrant {
        &self.grant
    }

    /// Its lifecycle state.
    pub const fn state(&self) -> ContainerGrantState {
        self.state
    }
}

/// The result of a confirmed revocation.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ContainerRevocation {
    /// The revoked grant.
    pub grant_id: Uuid,
    /// Whether this was the container's last grant, so its profile was deleted.
    pub profile_deleted: bool,
}

/// The stable container name for one workspace: `OMGCode.Workspace.` and the
/// workspace ID as 32 hex digits, 50 characters in all.
pub fn container_name(workspace: WorkspaceId) -> Result<ContainerName, ControlError> {
    ContainerName::new(&format!(
        "{CONTAINER_PREFIX}{}",
        workspace.as_uuid().simple()
    ))
    .map_err(|_| ControlError::Invalid("workspace container name is invalid"))
}

impl ControlStore {
    /// Every recorded grant that has not been confirmed revoked.
    pub fn container_grants(&self) -> &[ContainerGrantStatus] {
        &self.snapshot.container_grants
    }

    /// Grant the workspace container access to `path` and record it.
    ///
    /// Read-write grants are limited to the workspace directory. A read-execute
    /// grant must be the workspace, a directory inside it, or a directory
    /// outside the development root, and never overlaps application storage.
    /// The intent is synced before the ACL changes. If the change fails, the
    /// grant stays `pending` and must be revoked.
    pub fn grant_container_access(
        &mut self,
        workspace_id: WorkspaceId,
        path: &Path,
        access: ContainerGrantAccess,
        expected_revision: u64,
    ) -> Result<ContainerGrant, ControlError> {
        self.check_revision(expected_revision)?;
        if !CONTAINER_GRANTS_SUPPORTED {
            return Err(ControlError::Container(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "container grants exist only on Windows",
            )));
        }
        let workspace = self
            .workspace(workspace_id)
            .ok_or(ControlError::UnknownWorkspace)?
            .clone();
        self.check_directory_scope(&workspace.directory)?;
        let directory = DirectoryRecord::capture(path)?;
        self.check_storage_overlap(&directory)?;
        self.check_grant_scope(&workspace.directory, &directory, access)?;
        if self.snapshot.container_grants.iter().any(|status| {
            status.grant.workspace_id == workspace_id
                && status.grant.directory.same_directory(&directory)
        }) {
            return Err(ControlError::GrantRefused(
                "this directory already has a grant for the workspace; revoke it first",
            ));
        }
        let name = container_name(workspace_id)?;
        let grant = ContainerGrant {
            id: Uuid::new_v4(),
            workspace_id,
            container: name.as_str().to_owned(),
            sid: platform::container_sid(&name).map_err(ControlError::Container)?,
            directory,
            access,
        };
        self.append(
            Action::ContainerGrantRequested {
                grant: grant.clone(),
            },
            self.next_revision()?,
        )?;
        let live = grant.directory.live_path()?;
        platform::grant(
            &live,
            grant.directory.native_identity(),
            &name,
            access.into(),
        )
        .map_err(ControlError::Container)?;
        self.append(
            Action::ContainerGrantApplied { grant_id: grant.id },
            self.revision(),
        )?;
        Ok(grant)
    }

    /// Remove a recorded grant from its directory and record the outcome.
    ///
    /// This needs no settings revision and can run when the journal is
    /// otherwise full. A failed ACL change leaves the grant `revoking`; run the
    /// revocation again. The directory must still be the recorded one.
    pub fn revoke_container_grant(
        &mut self,
        grant_id: Uuid,
    ) -> Result<ContainerRevocation, ControlError> {
        self.ensure_healthy()?;
        let status = self
            .snapshot
            .container_grants
            .iter()
            .find(|status| status.grant.id == grant_id)
            .cloned()
            .ok_or(ControlError::UnknownGrant)?;
        let name = ContainerName::new(&status.grant.container)
            .map_err(|_| ControlError::Invalid("recorded container name is invalid"))?;
        if status.state != ContainerGrantState::Revoking {
            self.append(
                Action::ContainerRevokeRequested { grant_id },
                self.revision(),
            )?;
        }
        let live = status.grant.directory.live_path()?;
        platform::revoke(&live, status.grant.directory.native_identity(), &name)
            .map_err(ControlError::Container)?;
        self.append(Action::ContainerGrantRevoked { grant_id }, self.revision())?;
        let last = !self
            .snapshot
            .container_grants
            .iter()
            .any(|other| other.grant.container == status.grant.container);
        if last {
            platform::delete_profile(&name).map_err(ControlError::ProfileRemoval)?;
        }
        Ok(ContainerRevocation {
            grant_id,
            profile_deleted: last,
        })
    }

    /// Keep read-write grants on the workspace itself, and keep read grants
    /// from reaching other workspaces through the development root.
    fn check_grant_scope(
        &self,
        workspace: &DirectoryRecord,
        directory: &DirectoryRecord,
        access: ContainerGrantAccess,
    ) -> Result<(), ControlError> {
        if access == ContainerGrantAccess::ReadWrite {
            if !workspace.same_directory(directory) {
                return Err(ControlError::GrantRefused(
                    "read-write container grants are limited to the workspace directory",
                ));
            }
            return Ok(());
        }
        let root = self
            .snapshot
            .development_root
            .as_ref()
            .ok_or(ControlError::NoRoot)?;
        let overlaps_root = root.contains(directory)? || directory.contains(root)?;
        if overlaps_root && !workspace.contains(directory)? {
            return Err(ControlError::GrantRefused(
                "read grants under the development root must stay inside the workspace",
            ));
        }
        Ok(())
    }

    /// Events and bytes that must stay free so every outstanding grant can
    /// still reach a confirmed revocation.
    pub(super) fn grant_reserve(snapshot: &ControlSnapshot) -> (usize, usize) {
        let events: usize = snapshot
            .container_grants
            .iter()
            .map(|status| match status.state {
                ContainerGrantState::Pending => 3,
                ContainerGrantState::Active => 2,
                ContainerGrantState::Revoking => 1,
            })
            .sum();
        (events, events.saturating_mul(GRANT_EVENT_RESERVE_BYTES))
    }
}

/// Whether an action moves a grant toward revocation and may use the reserve.
pub(super) const fn uses_grant_reserve(action: &Action) -> bool {
    matches!(
        action,
        Action::ContainerGrantApplied { .. }
            | Action::ContainerRevokeRequested { .. }
            | Action::ContainerGrantRevoked { .. }
    )
}

/// Validate and apply one grant action during replay or append preflight.
pub(super) fn apply(state: &mut ControlSnapshot, action: &Action) -> Result<(), ControlError> {
    match action {
        Action::ContainerGrantRequested { grant } => {
            let workspace = state
                .workspaces
                .iter()
                .find(|workspace| workspace.id == grant.workspace_id)
                .ok_or(ControlError::Invalid("grant for an unknown workspace"))?;
            let per_workspace = state
                .container_grants
                .iter()
                .filter(|status| status.grant.workspace_id == grant.workspace_id)
                .count();
            if !valid_uuid(grant.id)
                || grant.container != container_name(grant.workspace_id)?.as_str()
                || !valid_sid(&grant.sid)
                || !grant.directory.validate_shape()
                || (grant.access == ContainerGrantAccess::ReadWrite
                    && !workspace.directory.same_directory(&grant.directory))
                || per_workspace >= MAX_GRANTS_PER_WORKSPACE
                || state.container_grants.len() >= MAX_GRANTS
                || state.container_grants.iter().any(|status| {
                    status.grant.id == grant.id
                        || (status.grant.workspace_id == grant.workspace_id
                            && status.grant.directory.same_directory(&grant.directory))
                })
            {
                return Err(ControlError::Invalid(
                    "invalid or duplicate container grant",
                ));
            }
            state.container_grants.push(ContainerGrantStatus {
                grant: grant.clone(),
                state: ContainerGrantState::Pending,
            });
        }
        Action::ContainerGrantApplied { grant_id } => {
            transition(state, *grant_id, |current| {
                (current == ContainerGrantState::Pending).then_some(ContainerGrantState::Active)
            })?;
        }
        Action::ContainerRevokeRequested { grant_id } => {
            transition(state, *grant_id, |current| {
                (current != ContainerGrantState::Revoking).then_some(ContainerGrantState::Revoking)
            })?;
        }
        Action::ContainerGrantRevoked { grant_id } => {
            let index = state
                .container_grants
                .iter()
                .position(|status| {
                    status.grant.id == *grant_id && status.state == ContainerGrantState::Revoking
                })
                .ok_or(ControlError::Invalid("revocation without a request"))?;
            state.container_grants.remove(index);
        }
        _ => {}
    }
    Ok(())
}

fn transition(
    state: &mut ControlSnapshot,
    grant_id: Uuid,
    next: impl FnOnce(ContainerGrantState) -> Option<ContainerGrantState>,
) -> Result<(), ControlError> {
    let status = state
        .container_grants
        .iter_mut()
        .find(|status| status.grant.id == grant_id)
        .ok_or(ControlError::Invalid("unknown container grant"))?;
    status.state = next(status.state).ok_or(ControlError::Invalid("invalid grant transition"))?;
    Ok(())
}

/// A container SID: `S-1-15-2-` followed by decimal sub-authorities.
fn valid_sid(text: &str) -> bool {
    text.len() <= MAX_SID_CHARS
        && text.strip_prefix("S-1-15-2-").is_some_and(|rest| {
            rest.split('-')
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        })
}

#[cfg(test)]
mod tests;
