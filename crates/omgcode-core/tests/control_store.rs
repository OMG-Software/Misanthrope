use std::fs;
use std::path::{Path, PathBuf};

use omgcode_core::control::{ControlError, ControlStore, WorkspaceRegistration};
use omgcode_core::policy::{DecisionReason, Operation, PermissionSet, PolicyRequest};
use tempfile::TempDir;
use uuid::Uuid;

/// An incomplete ancestry search must not establish storage separation.
#[test]
fn ancestor_limit_cannot_allow_development_root_inside_application_storage() {
    let temporary = tempfile::tempdir().expect("private temporary directory");
    let data = temporary.path().join("data");
    let mut store = ControlStore::open(&data).expect("control store");
    let mut deep_root = data.clone();
    for _ in 0..260 {
        deep_root.push("d");
    }
    std::fs::create_dir_all(&deep_root).expect("deep directory fixture");

    assert!(store.set_development_root(&deep_root, 0).is_err());
    assert_eq!(store.revision(), 0);
    assert!(!store.has_development_root());
    assert_eq!(store.audit(100).expect("audit unchanged").len(), 1);
}

const CONTROL_FILE: &str = "control-v1.jsonl";

/// A dangling directory entry must never select unconfigured compatibility mode.
#[cfg(unix)]
#[test]
fn dangling_control_link_is_invalid_state_not_absent_state() {
    let temporary = tempfile::tempdir().expect("private temporary directory");
    let data = temporary.path().join("data");
    fs::create_dir(&data).expect("application storage");
    let path = data.join(CONTROL_FILE);
    let absent_target = temporary.path().join("missing-target");
    std::os::unix::fs::symlink(&absent_target, &path).expect("dangling fixture link");

    assert!(ControlStore::open_existing(&data).is_err());
    assert!(ControlStore::open(&data).is_err());
    assert_eq!(fs::read_link(&path).expect("link preserved"), absent_target);
    assert!(!absent_target.exists());
}

struct Fixture {
    _temporary: TempDir,
    data: PathBuf,
    root: PathBuf,
    workspace: PathBuf,
    second_workspace: PathBuf,
}

/// Keep trusted application storage and both project folders in separate subtrees.
fn fixture() -> Fixture {
    let temporary = tempfile::tempdir().expect("private temporary directory");
    let data = temporary.path().join("app-data");
    let root = temporary.path().join("development-root");
    let workspace = root.join("workspace");
    let second_workspace = root.join("second-workspace");
    fs::create_dir(&root).expect("development root");
    fs::create_dir(&workspace).expect("workspace directory");
    fs::create_dir(&second_workspace).expect("second workspace directory");
    Fixture {
        _temporary: temporary,
        data,
        root,
        workspace,
        second_workspace,
    }
}

/// Initialize the real journal and explicitly select the fixture's development root.
fn open_store(fixture: &Fixture) -> ControlStore {
    let mut store = ControlStore::open(&fixture.data).expect("open control store");
    let revision = store.revision();
    store
        .set_development_root(&fixture.root, revision)
        .expect("select development root");
    store
}

/// Use the current revision so callers can isolate the behavior under test.
fn register(store: &mut ControlStore, path: &Path) -> WorkspaceRegistration {
    store
        .register_workspace(path, store.revision())
        .expect("register workspace")
}

fn session_id() -> Uuid {
    Uuid::new_v4()
}

fn control_path(data: &Path) -> PathBuf {
    data.join(CONTROL_FILE)
}

/// Compare closed-file bytes because invalid replay must never rewrite its evidence.
fn expect_invalid_unchanged(data: &Path) {
    let path = control_path(data);
    let before = fs::read(&path).expect("read tampered journal before open");
    let result = ControlStore::open_existing(data);
    assert!(matches!(result, Err(ControlError::Invalid(_))));
    let after = fs::read(&path).expect("read tampered journal after failed open");
    assert_eq!(
        after, before,
        "invalid control journal must remain unchanged"
    );
}

/// Registry identity and default flags must survive a fresh writer and replay.
#[test]
fn workspace_ids_and_default_flags_survive_reopen_and_reregistration_is_idempotent() {
    let fixture = fixture();
    let mut store = open_store(&fixture);
    let registration = register(&mut store, &fixture.workspace);
    let id = registration.id();
    assert_eq!(registration.permissions(), PermissionSet::default());
    let expected_revision = store.revision();
    drop(store);

    let before = fs::read(control_path(&fixture.data)).expect("read control journal");
    let mut reopened = ControlStore::open_existing(&fixture.data)
        .expect("open existing control journal")
        .expect("journal exists");
    let saved = reopened
        .workspace(id)
        .expect("workspace ID survives reopen");
    assert_eq!(saved.id(), id);
    assert_eq!(saved.permissions(), PermissionSet::default());
    assert_eq!(reopened.revision(), expected_revision);

    let repeated = reopened
        .register_workspace(&fixture.workspace, reopened.revision())
        .expect("registering the same directory is idempotent");
    assert_eq!(repeated.id(), id);
    assert_eq!(repeated.permissions(), PermissionSet::default());
    assert_eq!(reopened.revision(), expected_revision);
    drop(reopened);

    let after = fs::read(control_path(&fixture.data)).expect("read control journal after reopen");
    assert_eq!(
        after, before,
        "idempotent registration must not append an event"
    );
}

#[test]
fn stale_compare_and_set_fails_without_changing_journal_bytes() {
    let fixture = fixture();
    let store = open_store(&fixture);
    let current_revision = store.revision();
    drop(store);
    let before = fs::read(control_path(&fixture.data)).expect("read control journal");

    let mut reopened = ControlStore::open_existing(&fixture.data)
        .expect("open existing control journal")
        .expect("journal exists");
    let error = reopened
        .set_development_root(&fixture.root, current_revision.saturating_sub(1))
        .expect_err("stale compare-and-set must fail");
    assert!(matches!(error, ControlError::Conflict { .. }));
    assert_eq!(reopened.revision(), current_revision);
    drop(reopened);

    let after = fs::read(control_path(&fixture.data)).expect("read journal after conflict");
    assert_eq!(
        after, before,
        "a rejected compare-and-set must not append bytes"
    );
}

#[test]
fn enabled_configuration_is_audited_without_granting_execution_or_changing_revision() {
    let fixture = fixture();
    let mut store = open_store(&fixture);
    let registration = register(&mut store, &fixture.workspace);
    let permissions = PermissionSet {
        execute: true,
        ..PermissionSet::default()
    };
    store
        .set_permissions(registration.id(), permissions, store.revision())
        .expect("enable configured execute preference");
    let session = session_id();
    let binding = store
        .bind_session(session, registration.id())
        .expect("bind current workspace selection");
    let revision = store.revision();
    let audit_before = store.audit(100).expect("read prior audit records").len();

    let decision = store
        .evaluate(
            &binding,
            &PolicyRequest {
                session_id: session,
                workspace_id: registration.id(),
                operation: Operation::Execute,
            },
        )
        .expect("evaluate configured policy");

    assert!(decision.configured_allowed());
    assert!(!decision.execution_allowed());
    assert_eq!(decision.reason(), DecisionReason::Enabled);
    assert_eq!(
        store.revision(),
        revision,
        "audit-only evaluation keeps revision"
    );
    assert_eq!(
        store.audit(100).expect("read audit records").len(),
        audit_before + 1
    );
}

#[test]
fn session_and_workspace_mismatches_are_denied() {
    let fixture = fixture();
    let mut store = open_store(&fixture);
    let first = register(&mut store, &fixture.workspace);
    let second = register(&mut store, &fixture.second_workspace);
    let session = session_id();
    let binding = store
        .bind_session(session, first.id())
        .expect("bind first workspace");

    let wrong_session = store
        .evaluate(
            &binding,
            &PolicyRequest {
                session_id: session_id(),
                workspace_id: first.id(),
                operation: Operation::Read,
            },
        )
        .expect("audit session mismatch");
    assert_eq!(wrong_session.reason(), DecisionReason::SessionMismatch);
    assert!(!wrong_session.configured_allowed());
    assert!(!wrong_session.execution_allowed());

    let wrong_workspace = store
        .evaluate(
            &binding,
            &PolicyRequest {
                session_id: session,
                workspace_id: second.id(),
                operation: Operation::Read,
            },
        )
        .expect("audit workspace mismatch");
    assert_eq!(wrong_workspace.reason(), DecisionReason::WorkspaceMismatch);
    assert!(!wrong_workspace.configured_allowed());
    assert!(!wrong_workspace.execution_allowed());
}

#[test]
fn changing_development_root_makes_existing_binding_stale() {
    let fixture = fixture();
    let second_root = fixture._temporary.path().join("second-root");
    fs::create_dir(&second_root).expect("second development root");
    let mut store = open_store(&fixture);
    let workspace = register(&mut store, &fixture.workspace);
    let session = session_id();
    let binding = store
        .bind_session(session, workspace.id())
        .expect("bind workspace before root change");

    let revision = store.revision();
    store
        .set_development_root(&second_root, revision)
        .expect("change development root");
    let decision = store
        .evaluate(
            &binding,
            &PolicyRequest {
                session_id: session,
                workspace_id: workspace.id(),
                operation: Operation::Read,
            },
        )
        .expect("audit stale binding");

    assert_eq!(decision.reason(), DecisionReason::StaleRevision);
    assert!(!decision.configured_allowed());
    assert!(!decision.execution_allowed());
}

#[test]
fn a_binding_from_another_store_is_denied() {
    let fixture = fixture();
    let second_data = fixture._temporary.path().join("second-app-data");
    let mut first_store = open_store(&fixture);
    let first_workspace = register(&mut first_store, &fixture.workspace);
    let session = session_id();
    let binding = first_store
        .bind_session(session, first_workspace.id())
        .expect("bind first store workspace");

    let mut second_store = ControlStore::open(&second_data).expect("open second control store");
    let revision = second_store.revision();
    second_store
        .set_development_root(&fixture.root, revision)
        .expect("select same development root in second store");
    register(&mut second_store, &fixture.workspace);
    let decision = second_store
        .evaluate(
            &binding,
            &PolicyRequest {
                session_id: session,
                workspace_id: first_workspace.id(),
                operation: Operation::Read,
            },
        )
        .expect("audit foreign store binding");

    assert_eq!(decision.reason(), DecisionReason::StoreMismatch);
    assert!(!decision.configured_allowed());
    assert!(!decision.execution_allowed());
}

/// New objects at a saved path must not inherit its non-default permission choices.
#[test]
fn replacing_workspace_directory_blocks_binding_and_reregistration() {
    let fixture = fixture();
    let mut store = open_store(&fixture);
    let workspace = register(&mut store, &fixture.workspace);
    let permissions = PermissionSet {
        read: false,
        ..PermissionSet::default()
    };
    store
        .set_permissions(workspace.id(), permissions, store.revision())
        .expect("set nondefault workspace preferences");
    let revision = store.revision();
    let old_path = fixture._temporary.path().join("old-workspace");
    fs::rename(&fixture.workspace, &old_path).expect("move registered directory away");
    fs::create_dir(&fixture.workspace).expect("create replacement directory at old path");

    let bind_error = store
        .bind_session(session_id(), workspace.id())
        .expect_err("replacement must not bind to the old registration");
    assert!(matches!(bind_error, ControlError::DirectoryChanged));
    let register_error = store
        .register_workspace(&fixture.workspace, revision)
        .expect_err("replacement path must not inherit the old registration");
    assert!(matches!(register_error, ControlError::DirectoryChanged));
    assert_eq!(store.revision(), revision);
    assert_eq!(
        store
            .workspace(workspace.id())
            .expect("old registration remains descriptive")
            .permissions(),
        permissions
    );
}

/// Renaming an object must not return an ID whose saved path is unavailable.
#[test]
fn renamed_workspace_cannot_reuse_registration_with_a_missing_saved_path() {
    let fixture = fixture();
    let mut store = open_store(&fixture);
    let registration = register(&mut store, &fixture.workspace);
    let renamed = fixture.root.join("renamed-workspace");
    let revision = store.revision();
    let audit_before = serde_json::to_value(store.audit(100).expect("audit before rename"))
        .expect("serialize audit");
    fs::rename(&fixture.workspace, &renamed).expect("rename workspace within root");

    assert!(matches!(
        store.register_workspace(&renamed, revision),
        Err(ControlError::DirectoryChanged)
    ));
    assert_eq!(store.revision(), revision);
    assert_eq!(
        serde_json::to_value(store.audit(100).expect("unchanged audit")).expect("serialize audit"),
        audit_before
    );
    assert!(store.workspace(registration.id()).is_some());
    assert!(matches!(
        store.bind_session(session_id(), registration.id()),
        Err(ControlError::DirectoryChanged)
    ));
}

/// An open Unix journal can move; stale app-data paths cannot prove separation.
#[cfg(unix)]
#[test]
fn moved_application_storage_is_rejected_before_root_overlap_check() {
    let fixture = fixture();
    let mut store = ControlStore::open(&fixture.data).expect("control store");
    let moved = fixture.root.join("moved-app-data");
    fs::rename(&fixture.data, &moved).expect("move storage while its writer is open");

    assert!(matches!(
        store.set_development_root(&fixture.root, 0),
        Err(ControlError::DirectoryChanged)
    ));
    assert_eq!(store.revision(), 0);
    assert!(!store.has_development_root());
    assert_eq!(store.audit(100).expect("unchanged audit").len(), 1);
    drop(store);
    let reopened = ControlStore::open_existing(&moved)
        .expect("original journal remains valid")
        .expect("moved journal exists");
    assert_eq!(reopened.revision(), 0);
    assert!(!reopened.has_development_root());
}

/// Registering a sibling of the development root must not widen configured scope.
#[test]
fn registration_outside_the_development_root_is_rejected() {
    let fixture = fixture();
    let outside = fixture._temporary.path().join("outside-workspace");
    fs::create_dir(&outside).expect("outside workspace directory");
    let mut store = open_store(&fixture);

    let error = store
        .register_workspace(&outside, store.revision())
        .expect_err("outside workspace must be rejected");
    assert!(matches!(error, ControlError::DirectoryChanged));
}

/// Reject either ancestor relationship between task data and application storage.
#[test]
fn application_storage_overlap_is_rejected_in_both_directions() {
    let temporary = tempfile::tempdir().expect("private temporary directory");
    let root_containing_data = temporary.path().join("root-containing-data");
    let data_inside_root = root_containing_data.join("app-data");
    fs::create_dir_all(&data_inside_root).expect("app data below development root");
    let mut store = ControlStore::open(&data_inside_root).expect("open nested app data");
    let error = store
        .set_development_root(&root_containing_data, store.revision())
        .expect_err("root containing application storage must be rejected");
    assert!(matches!(error, ControlError::StorageOverlap));
    drop(store);

    let data_containing_root = temporary.path().join("data-containing-root");
    let nested_root = data_containing_root.join("development-root");
    fs::create_dir_all(&nested_root).expect("development root below app data");
    let mut nested_store = ControlStore::open(&data_containing_root).expect("open app data");
    let error = nested_store
        .set_development_root(&nested_root, nested_store.revision())
        .expect_err("application storage below development root must be rejected");
    assert!(matches!(error, ControlError::StorageOverlap));
}

/// Compatibility detection must not create storage when the control file is absent.
#[test]
fn opening_missing_existing_store_does_not_create_files_or_directories() {
    let temporary = tempfile::tempdir().expect("private temporary directory");
    let missing = temporary.path().join("missing").join("app-data");

    assert!(
        ControlStore::open_existing(&missing)
            .expect("missing store is not an error")
            .is_none()
    );
    assert!(
        !missing.exists(),
        "open_existing must not create the data directory"
    );
    assert!(!control_path(&missing).exists());
}

/// Independent handles cannot concurrently mutate one settings source of truth.
#[test]
fn a_second_control_store_writer_receives_busy() {
    let fixture = fixture();
    let first = ControlStore::open(&fixture.data).expect("open first writer");
    let error = match ControlStore::open(&fixture.data) {
        Err(error) => error,
        Ok(second) => {
            drop(second);
            panic!("second writer must be denied");
        }
    };

    assert!(matches!(error, ControlError::Busy));
    drop(first);
}

/// Invalid persisted control state must fail without automatic reset or repair.
#[test]
fn malformed_control_journals_fail_without_repair() {
    for corruption in ["duplicate-key", "unknown-schema", "truncated-tail", "empty"] {
        let temporary = tempfile::tempdir().expect("private temporary directory");
        let data = temporary.path().join("app-data");
        let store = ControlStore::open(&data).expect("initialize control journal");
        drop(store);
        let path = control_path(&data);
        let original = fs::read(&path).expect("read initialized control journal");
        let damaged = match corruption {
            "duplicate-key" => {
                let text = std::str::from_utf8(&original).expect("JSONL is UTF-8");
                let first_line = text.lines().next().expect("initial event line");
                format!("{{\"schema_version\":1,{}\n", &first_line[1..]).into_bytes()
            }
            "unknown-schema" => {
                let text = std::str::from_utf8(&original).expect("JSONL is UTF-8");
                let changed = text.replacen("\"schema_version\":1", "\"schema_version\":2", 1);
                assert_ne!(
                    changed, text,
                    "initialized event must include the schema version"
                );
                changed.into_bytes()
            }
            "truncated-tail" => original[..original.len() - 1].to_vec(),
            "empty" => Vec::new(),
            _ => unreachable!("test case is listed above"),
        };
        fs::write(&path, &damaged).expect("write invalid journal fixture");
        expect_invalid_unchanged(&data);
    }
}

/// One workspace's package cache root is a revisioned setting. It must be
/// existing, outside application storage, outside the development root, and
/// not overlap another workspace's cache root.
#[test]
fn package_cache_root_is_recorded_replayed_cleared_and_scoped() {
    let fixture = fixture();
    let cache_root = fixture._temporary.path().join("package-cache-root");
    fs::create_dir(&cache_root).expect("cache root");
    let nested = cache_root.join("nested");
    fs::create_dir(&nested).expect("nested cache root");
    let inside = fixture.root.join("cache-inside");
    fs::create_dir(&inside).expect("inside development root");
    let mut store = ControlStore::open(&fixture.data).expect("store");
    store
        .set_development_root(&fixture.root, 0)
        .expect("development root");
    let workspace_id = store
        .register_workspace(&fixture.workspace, store.revision())
        .expect("workspace")
        .id();
    let second_id = store
        .register_workspace(&fixture.second_workspace, store.revision())
        .expect("second workspace")
        .id();
    let revision = store.revision();
    store
        .set_package_cache_root(workspace_id, &cache_root, revision)
        .expect("cache root");
    assert!(
        serde_json::to_value(store.snapshot())
            .expect("snapshot")
            .get("package_cache_roots")
            .is_some()
    );
    let revision = store.revision();
    assert!(matches!(
        store.set_package_cache_root(workspace_id, &inside, revision),
        Err(ControlError::PackageCacheRefused(_))
    ));
    assert!(matches!(
        store.set_package_cache_root(workspace_id, &fixture.data, revision),
        Err(ControlError::StorageOverlap)
    ));
    assert!(matches!(
        store.set_package_cache_root(second_id, &nested, revision),
        Err(ControlError::PackageCacheRefused(_))
    ));
    let missing = fixture._temporary.path().join("missing-cache");
    assert!(
        store
            .set_package_cache_root(workspace_id, &missing, revision)
            .is_err()
    );
    drop(store);

    let mut store = ControlStore::open(&fixture.data).expect("replay");
    assert!(
        serde_json::to_value(store.snapshot())
            .expect("snapshot")
            .get("package_cache_roots")
            .is_some()
    );
    let revision = store.revision();
    store
        .clear_package_cache_root(workspace_id, revision)
        .expect("clear cache root");
    assert!(
        serde_json::to_value(store.snapshot())
            .expect("snapshot")
            .get("package_cache_roots")
            .is_none()
    );
    let revision = store.revision();
    store
        .clear_package_cache_root(workspace_id, revision)
        .expect("idempotent clear");
    assert_eq!(store.revision(), revision);
}
