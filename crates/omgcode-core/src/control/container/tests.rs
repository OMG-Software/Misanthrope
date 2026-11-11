//! Container grant journal tests. Windows tests change real ACLs on temporary
//! directories and delete the container profile they create.

use std::fs;
use std::path::{Path, PathBuf};

use super::super::{Action, ControlError, ControlStore, MAX_BYTES};
use super::*;

struct Scene {
    _directory: tempfile::TempDir,
    data: PathBuf,
    root: PathBuf,
    workspace: PathBuf,
    other: PathBuf,
    toolchain: PathBuf,
}

impl Scene {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let data = directory.path().join("data");
        let root = directory.path().join("projects");
        let workspace = root.join("app");
        let other = root.join("other");
        let toolchain = directory.path().join("toolchain");
        for path in [&root, &workspace, &other, &toolchain] {
            fs::create_dir(path).expect("scene directory");
        }
        Self {
            _directory: directory,
            data,
            root,
            workspace,
            other,
            toolchain,
        }
    }

    /// A store with the root and two registered workspaces, at revision 3.
    fn store(&self) -> (ControlStore, WorkspaceId) {
        let mut store = ControlStore::open(&self.data).expect("control store");
        store.set_development_root(&self.root, 0).expect("root");
        let workspace = store
            .register_workspace(&self.workspace, 1)
            .expect("workspace");
        store.register_workspace(&self.other, 2).expect("other");
        (store, workspace.id())
    }
}

fn grant_for(
    workspace_id: WorkspaceId,
    path: &Path,
    access: ContainerGrantAccess,
) -> ContainerGrant {
    ContainerGrant {
        id: Uuid::new_v4(),
        workspace_id,
        container: container_name(workspace_id)
            .expect("name")
            .as_str()
            .to_owned(),
        sid: "S-1-15-2-1-2-3-4-5-6-7".to_owned(),
        directory: DirectoryRecord::capture(path).expect("directory"),
        access,
    }
}

#[test]
fn workspace_container_names_are_stable_and_distinct() {
    let first = WorkspaceId::fresh();
    let name = container_name(first).expect("name");
    assert_eq!(name, container_name(first).expect("again"));
    assert_ne!(name, container_name(WorkspaceId::fresh()).expect("other"));
    assert_eq!(name.as_str().len(), 50);
    assert!(name.as_str().starts_with("OMGCode.Workspace."));
}

#[test]
fn replay_refuses_inconsistent_grant_records() {
    let scene = Scene::new();
    let (store, workspace) = scene.store();
    let base = store.snapshot();
    let valid = grant_for(workspace, &scene.workspace, ContainerGrantAccess::ReadWrite);
    let mut wrong_name = valid.clone();
    wrong_name.container = "OMGCode.Workspace.other".to_owned();
    let mut wrong_sid = valid.clone();
    wrong_sid.sid = "S-1-5-21-1".to_owned();
    let mut nil_id = valid.clone();
    nil_id.id = Uuid::nil();
    let outside_write = grant_for(workspace, &scene.toolchain, ContainerGrantAccess::ReadWrite);
    let unknown_workspace = grant_for(
        WorkspaceId::fresh(),
        &scene.toolchain,
        ContainerGrantAccess::ReadExecute,
    );
    for (name, grant) in [
        ("container name", wrong_name),
        ("SID", wrong_sid),
        ("identifier", nil_id),
        ("read-write outside the workspace", outside_write),
        ("unknown workspace", unknown_workspace),
    ] {
        let mut state = base.clone();
        assert!(
            matches!(
                apply(&mut state, &Action::ContainerGrantRequested { grant }),
                Err(ControlError::Invalid(_))
            ),
            "{name}"
        );
        assert!(state.container_grants.is_empty(), "{name}");
    }

    let mut state = base.clone();
    let id = valid.id;
    apply(
        &mut state,
        &Action::ContainerGrantRequested {
            grant: valid.clone(),
        },
    )
    .expect("request");
    assert!(
        apply(
            &mut state,
            &Action::ContainerGrantRequested { grant: valid }
        )
        .is_err(),
        "duplicate"
    );
    for (name, action) in [
        (
            "revoked before requested",
            Action::ContainerGrantRevoked { grant_id: id },
        ),
        (
            "unknown applied",
            Action::ContainerGrantApplied {
                grant_id: Uuid::new_v4(),
            },
        ),
    ] {
        assert!(apply(&mut state.clone(), &action).is_err(), "{name}");
    }
    apply(&mut state, &Action::ContainerGrantApplied { grant_id: id }).expect("applied");
    assert!(
        apply(
            &mut state.clone(),
            &Action::ContainerGrantApplied { grant_id: id }
        )
        .is_err(),
        "applied twice"
    );
    apply(
        &mut state,
        &Action::ContainerRevokeRequested { grant_id: id },
    )
    .expect("revoking");
    assert!(
        apply(
            &mut state.clone(),
            &Action::ContainerRevokeRequested { grant_id: id }
        )
        .is_err(),
        "revocation requested twice"
    );
    apply(&mut state, &Action::ContainerGrantRevoked { grant_id: id }).expect("revoked");
    assert!(state.container_grants.is_empty());
}

#[test]
fn grant_outcomes_keep_the_revision_and_requests_advance_it() {
    let scene = Scene::new();
    let (mut store, workspace) = scene.store();
    let grant = grant_for(
        workspace,
        &scene.toolchain,
        ContainerGrantAccess::ReadExecute,
    );
    let id = grant.id;
    let revision = store.revision();
    store
        .append(
            Action::ContainerGrantRequested { grant },
            store.next_revision().expect("revision"),
        )
        .expect("request");
    assert_eq!(store.revision(), revision + 1);
    for action in [
        Action::ContainerGrantApplied { grant_id: id },
        Action::ContainerRevokeRequested { grant_id: id },
        Action::ContainerGrantRevoked { grant_id: id },
    ] {
        store.append(action, store.revision()).expect("outcome");
        assert_eq!(store.revision(), revision + 1);
    }
    drop(store);
    let reopened = ControlStore::open_existing(&scene.data)
        .expect("replay")
        .expect("store");
    assert_eq!(reopened.revision(), revision + 1);
    assert!(reopened.container_grants().is_empty());
}

#[test]
fn outstanding_grants_keep_room_to_be_revoked() {
    let scene = Scene::new();
    let (mut store, workspace) = scene.store();
    let grant = grant_for(
        workspace,
        &scene.toolchain,
        ContainerGrantAccess::ReadExecute,
    );
    let id = grant.id;
    store
        .append(
            Action::ContainerGrantRequested { grant },
            store.next_revision().expect("revision"),
        )
        .expect("request");
    // Leave just the reserve for one pending grant plus a little.
    store.bytes = MAX_BYTES - 3 * GRANT_EVENT_RESERVE_BYTES - 64;
    let revision = store.revision();
    let mut changed = store.workspace(workspace).expect("workspace").permissions();
    changed.write = !changed.write;
    assert!(matches!(
        store.set_permissions(workspace, changed, revision),
        Err(ControlError::LimitReached)
    ));
    for action in [
        Action::ContainerGrantApplied { grant_id: id },
        Action::ContainerRevokeRequested { grant_id: id },
        Action::ContainerGrantRevoked { grant_id: id },
    ] {
        store
            .append(action, store.revision())
            .expect("revocation path uses the reserve");
    }
    assert!(store.container_grants().is_empty());
}

#[cfg(not(windows))]
#[test]
fn other_platforms_refuse_grants_without_recording_them() {
    let scene = Scene::new();
    let (mut store, workspace) = scene.store();
    let events = store.entries.len();
    let error = store
        .grant_container_access(
            workspace,
            &scene.workspace,
            ContainerGrantAccess::ReadWrite,
            store.revision(),
        )
        .expect_err("no AppContainer");
    assert!(
        matches!(&error, ControlError::Container(cause) if cause.kind() == std::io::ErrorKind::Unsupported),
        "{error}"
    );
    assert_eq!(store.entries.len(), events);
}

#[cfg(windows)]
mod windows {
    use super::super::super::AuditEntry;
    use super::*;
    use omgcode_platform::container::{ContainerAccess, granted_access, profile_exists};

    fn access(path: &Path, workspace: WorkspaceId) -> Option<ContainerAccess> {
        granted_access(path, &container_name(workspace).expect("name")).expect("access")
    }

    fn actions(entries: &[AuditEntry]) -> Vec<&'static str> {
        entries
            .iter()
            .map(|entry| match entry.action {
                Action::ContainerGrantRequested { .. } => "requested",
                Action::ContainerGrantApplied { .. } => "applied",
                Action::ContainerRevokeRequested { .. } => "revoke-requested",
                Action::ContainerGrantRevoked { .. } => "revoked",
                _ => "other",
            })
            .collect()
    }

    #[test]
    fn grants_are_recorded_applied_replayed_and_revoked() {
        let scene = Scene::new();
        let (mut store, workspace) = scene.store();
        let name = container_name(workspace).expect("name");
        let start = store.entries.len();
        let revision = store.revision();

        let write = store
            .grant_container_access(
                workspace,
                &scene.workspace,
                ContainerGrantAccess::ReadWrite,
                revision,
            )
            .expect("workspace grant");
        let read = store
            .grant_container_access(
                workspace,
                &scene.toolchain,
                ContainerGrantAccess::ReadExecute,
                revision + 1,
            )
            .expect("toolchain grant");
        assert_eq!(store.revision(), revision + 2);
        assert_eq!(
            access(&scene.workspace, workspace),
            Some(ContainerAccess::ReadWrite)
        );
        assert_eq!(
            access(&scene.toolchain, workspace),
            Some(ContainerAccess::ReadExecute)
        );
        assert_eq!(access(&scene.other, workspace), None);
        assert!(profile_exists(&name).expect("profile"));
        assert_eq!(
            actions(&store.entries[start..]),
            ["requested", "applied", "requested", "applied"]
        );

        drop(store);
        let mut store = ControlStore::open_existing(&scene.data)
            .expect("replay")
            .expect("store");
        let states: Vec<_> = store
            .container_grants()
            .iter()
            .map(|status| (status.grant().id(), status.state()))
            .collect();
        assert_eq!(
            states,
            [
                (write.id(), ContainerGrantState::Active),
                (read.id(), ContainerGrantState::Active)
            ]
        );
        let status = serde_json::to_value(store.snapshot()).expect("status");
        assert_eq!(status["container_grants"][0]["state"], "active");
        assert_eq!(status["container_grants"][0]["access"], "read_write");

        let first = store
            .revoke_container_grant(write.id())
            .expect("revoke write");
        assert!(!first.profile_deleted);
        assert_eq!(access(&scene.workspace, workspace), None);
        assert!(profile_exists(&name).expect("profile"));
        let last = store
            .revoke_container_grant(read.id())
            .expect("revoke read");
        assert!(last.profile_deleted);
        assert_eq!(access(&scene.toolchain, workspace), None);
        assert!(!profile_exists(&name).expect("profile"));
        assert_eq!(
            store.revision(),
            revision + 2,
            "revocation needs no revision"
        );
        assert!(store.container_grants().is_empty());
        assert!(matches!(
            store.revoke_container_grant(read.id()),
            Err(ControlError::UnknownGrant)
        ));
    }

    #[test]
    fn grants_outside_the_allowed_scope_are_refused_before_recording() {
        let scene = Scene::new();
        let (mut store, workspace) = scene.store();
        let nested = scene.workspace.join("nested");
        fs::create_dir(&nested).expect("nested");
        let events = store.entries.len();
        let revision = store.revision();
        let cases: [(&str, &Path, ContainerGrantAccess); 5] = [
            (
                "write below the workspace",
                &nested,
                ContainerGrantAccess::ReadWrite,
            ),
            (
                "write outside",
                &scene.toolchain,
                ContainerGrantAccess::ReadWrite,
            ),
            (
                "read another workspace",
                &scene.other,
                ContainerGrantAccess::ReadExecute,
            ),
            (
                "read the root",
                &scene.root,
                ContainerGrantAccess::ReadExecute,
            ),
            (
                "read an ancestor of the root",
                scene.root.parent().expect("parent"),
                ContainerGrantAccess::ReadExecute,
            ),
        ];
        for (name, path, grant_access) in cases {
            let error = store
                .grant_container_access(workspace, path, grant_access, revision)
                .expect_err(name);
            assert!(
                matches!(
                    error,
                    ControlError::GrantRefused(_) | ControlError::StorageOverlap
                ),
                "{name}: {error}"
            );
        }
        assert!(matches!(
            store.grant_container_access(
                workspace,
                &scene.data,
                ContainerGrantAccess::ReadExecute,
                revision
            ),
            Err(ControlError::StorageOverlap)
        ));
        assert!(matches!(
            store.grant_container_access(
                workspace,
                &scene.toolchain,
                ContainerGrantAccess::ReadExecute,
                revision - 1
            ),
            Err(ControlError::Conflict { .. })
        ));
        assert!(matches!(
            store.grant_container_access(
                WorkspaceId::fresh(),
                &scene.toolchain,
                ContainerGrantAccess::ReadExecute,
                revision
            ),
            Err(ControlError::UnknownWorkspace)
        ));
        assert_eq!(store.entries.len(), events);
        assert_eq!(access(&scene.toolchain, workspace), None);

        let inside = store
            .grant_container_access(
                workspace,
                &nested,
                ContainerGrantAccess::ReadExecute,
                revision,
            )
            .expect("read inside the workspace");
        assert!(matches!(
            store.grant_container_access(
                workspace,
                &nested,
                ContainerGrantAccess::ReadExecute,
                store.revision()
            ),
            Err(ControlError::GrantRefused(_))
        ));
        store.revoke_container_grant(inside.id()).expect("revoke");
    }

    #[test]
    fn a_pending_grant_with_an_unknown_outcome_can_be_revoked() {
        let scene = Scene::new();
        let (mut store, workspace) = scene.store();
        let name = container_name(workspace).expect("name");
        let mut grant = grant_for(
            workspace,
            &scene.toolchain,
            ContainerGrantAccess::ReadExecute,
        );
        grant.sid = omgcode_platform::container::container_sid(&name).expect("sid");
        let id = grant.id;
        // The intent was recorded and the ACL change happened, but the
        // outcome event was never written.
        omgcode_platform::container::grant(
            &scene.toolchain,
            grant.directory.native_identity(),
            &name,
            ContainerAccess::ReadExecute,
        )
        .expect("platform grant");
        store
            .append(
                Action::ContainerGrantRequested { grant },
                store.next_revision().expect("revision"),
            )
            .expect("request");
        drop(store);
        let mut store = ControlStore::open_existing(&scene.data)
            .expect("replay")
            .expect("store");
        assert_eq!(
            store.container_grants()[0].state(),
            ContainerGrantState::Pending
        );
        let revocation = store.revoke_container_grant(id).expect("revoke");
        assert!(revocation.profile_deleted);
        assert_eq!(access(&scene.toolchain, workspace), None);
        assert!(!profile_exists(&name).expect("profile"));
    }

    #[test]
    fn a_failed_revocation_stays_revoking_and_can_be_retried() {
        let scene = Scene::new();
        let (mut store, workspace) = scene.store();
        let grant = store
            .grant_container_access(
                workspace,
                &scene.toolchain,
                ContainerGrantAccess::ReadExecute,
                store.revision(),
            )
            .expect("grant");
        let moved = scene.toolchain.with_file_name("moved-toolchain");
        fs::rename(&scene.toolchain, &moved).expect("move");
        assert!(store.revoke_container_grant(grant.id()).is_err());
        assert_eq!(
            store.container_grants()[0].state(),
            ContainerGrantState::Revoking
        );
        assert_eq!(
            access(&moved, workspace),
            Some(ContainerAccess::ReadExecute)
        );
        fs::rename(&moved, &scene.toolchain).expect("restore");
        let events = store.entries.len();
        store.revoke_container_grant(grant.id()).expect("retry");
        assert_eq!(
            actions(&store.entries[events..]),
            ["revoked"],
            "a retry does not request revocation twice"
        );
        assert_eq!(access(&scene.toolchain, workspace), None);
    }
}
