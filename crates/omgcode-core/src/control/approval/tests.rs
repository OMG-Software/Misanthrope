use super::*;
use std::fs;

use crate::control::{
    ApprovalBinding, ApprovalDeclaration, ApprovalOperation, ApprovalState, ControlError,
    ControlStore, ProviderProfileDeclaration,
};
use crate::policy::WorkspaceId;
use uuid::Uuid;

fn scene() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    WorkspaceId,
    ControlStore,
) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let data = directory.path().join("data");
    let root = directory.path().join("dev");
    let workspace = root.join("work");
    fs::create_dir_all(&workspace).expect("workspace");
    let mut store = ControlStore::open(&data).expect("store");
    store
        .set_development_root(&root, 0)
        .expect("development root");
    let workspace_id = store
        .register_workspace(&workspace, store.revision())
        .expect("workspace")
        .id();
    (directory, data, workspace_id, store)
}

const NOW: u64 = 1_000_000;
const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn declaration(workspace_id: WorkspaceId, session_id: Uuid) -> ApprovalDeclaration {
    ApprovalDeclaration {
        session_id,
        workspace_id,
        operation: ApprovalOperation::Command,
        request_sha256: DIGEST.into(),
        summary: "Run the project build".into(),
        ttl_secs: 900,
    }
}

fn binding(workspace_id: WorkspaceId, session_id: Uuid) -> ApprovalBinding {
    ApprovalBinding {
        session_id,
        workspace_id,
        operation: ApprovalOperation::Command,
        request_sha256: DIGEST.into(),
    }
}

#[test]
fn an_approval_is_requested_decided_consumed_once() {
    let (_directory, _data, workspace_id, mut store) = scene();
    let session_id = Uuid::new_v4();
    let revision = store.revision();
    let approval = store
        .request_approval(&declaration(workspace_id, session_id), NOW)
        .expect("request");
    assert_eq!(approval.state, ApprovalState::Pending);
    assert_eq!(approval.created_unix, NOW);
    assert_eq!(approval.expires_unix, NOW + 900);
    assert_eq!(store.revision(), revision, "a request preserves revision");
    store
        .decide_approval(approval.id, true, NOW + 1)
        .expect("approve");
    assert_eq!(
        store
            .approval_status(approval.id, NOW + 2)
            .expect("status")
            .state,
        ApprovalState::Approved
    );
    store
        .consume_approval(approval.id, &binding(workspace_id, session_id), NOW + 3)
        .expect("consume");
    assert_eq!(
        store
            .approval_status(approval.id, NOW + 4)
            .expect("status")
            .state,
        ApprovalState::Consumed
    );
    assert!(matches!(
        store.consume_approval(approval.id, &binding(workspace_id, session_id), NOW + 5),
        Err(ControlError::ApprovalRefused(_))
    ));
}

#[test]
fn a_denied_or_expired_approval_cannot_be_consumed() {
    let (_directory, _data, workspace_id, mut store) = scene();
    let session_id = Uuid::new_v4();
    let denied = store
        .request_approval(&declaration(workspace_id, session_id), NOW)
        .expect("request");
    store
        .decide_approval(denied.id, false, NOW + 1)
        .expect("deny");
    assert_eq!(
        store
            .approval_status(denied.id, NOW + 2)
            .expect("status")
            .state,
        ApprovalState::Denied
    );
    assert!(matches!(
        store.consume_approval(denied.id, &binding(workspace_id, session_id), NOW + 3),
        Err(ControlError::ApprovalRefused(_))
    ));

    let expired = store
        .request_approval(
            &ApprovalDeclaration {
                ttl_secs: MIN_APPROVAL_TTL_SECS,
                ..declaration(workspace_id, session_id)
            },
            NOW,
        )
        .expect("short request");
    assert_eq!(
        store
            .approval_status(expired.id, NOW + MIN_APPROVAL_TTL_SECS)
            .expect("status")
            .state,
        ApprovalState::Expired
    );
    assert!(matches!(
        store.decide_approval(expired.id, true, NOW + MIN_APPROVAL_TTL_SECS),
        Err(ControlError::ApprovalRefused(_))
    ));
}

#[test]
fn binding_and_settings_changes_refuse_consumption() {
    let (_directory, _data, workspace_id, mut store) = scene();
    let session_id = Uuid::new_v4();
    let approval = store
        .request_approval(&declaration(workspace_id, session_id), NOW)
        .expect("request");
    store
        .decide_approval(approval.id, true, NOW + 1)
        .expect("approve");

    let mut wrong = binding(workspace_id, session_id);
    wrong.operation = ApprovalOperation::FileWrite;
    assert!(matches!(
        store.consume_approval(approval.id, &wrong, NOW + 2),
        Err(ControlError::ApprovalRefused(_))
    ));
    let mut wrong = binding(workspace_id, session_id);
    wrong.request_sha256 = "b".repeat(64);
    assert!(matches!(
        store.consume_approval(approval.id, &wrong, NOW + 2),
        Err(ControlError::ApprovalRefused(_))
    ));

    // A settings change after approval invalidates the recorded revision.
    store
        .add_provider_profile(
            &ProviderProfileDeclaration {
                name: "work".into(),
                kind: "openai".into(),
                auth: "api_key".into(),
            },
            store.revision(),
        )
        .expect("settings change");
    assert!(matches!(
        store.consume_approval(approval.id, &binding(workspace_id, session_id), NOW + 3),
        Err(ControlError::ApprovalRefused(_))
    ));
}

#[test]
fn approvals_survive_replay_and_refuse_invalid_requests() {
    let (_directory, data, workspace_id, mut store) = scene();
    let session_id = Uuid::new_v4();
    let approval = store
        .request_approval(&declaration(workspace_id, session_id), NOW)
        .expect("request");
    store
        .decide_approval(approval.id, true, NOW + 1)
        .expect("approve");
    let revision = store.revision();
    drop(store);

    let mut store = ControlStore::open(&data).expect("replay");
    assert_eq!(store.revision(), revision);
    assert_eq!(
        store
            .approval_status(approval.id, NOW + 2)
            .expect("status")
            .state,
        ApprovalState::Approved
    );
    assert_eq!(store.approval_statuses(NOW + 2).len(), 1);

    for declaration in [
        ApprovalDeclaration {
            request_sha256: "not-a-digest".into(),
            ..declaration(workspace_id, session_id)
        },
        ApprovalDeclaration {
            summary: String::new(),
            ..declaration(workspace_id, session_id)
        },
        ApprovalDeclaration {
            ttl_secs: 1,
            ..declaration(workspace_id, session_id)
        },
    ] {
        assert!(matches!(
            store.request_approval(&declaration, NOW),
            Err(ControlError::ApprovalRefused(_))
        ));
    }
    assert!(matches!(
        store.request_approval(&declaration(WorkspaceId::fresh(), session_id), NOW),
        Err(ControlError::UnknownWorkspace)
    ));
    assert!(matches!(
        store.decide_approval(Uuid::new_v4(), true, NOW),
        Err(ControlError::UnknownApproval)
    ));
}
