use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::*;
use crate::control::ControlStore;
use crate::policy::{PermissionSet, SessionBinding, WorkspaceId};
use crate::replacement::{
    CheckpointError, CheckpointInspection, RecordedArtifact, ReplacementOutcome,
    ReplacementProgress,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use uuid::Uuid;

const ORIGINAL: &[u8] = b"captured original bytes\n";
const PROPOSED: &[u8] = b"complete proposed replacement\n";

struct Fixture {
    data: PathBuf,
    workspace: PathBuf,
    workspace_id: WorkspaceId,
    session_id: Uuid,
    control: ControlStore,
    _temporary: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("create isolated fixture");
        let data = temporary.path().join("application-data");
        let root = temporary.path().join("development-root");
        let workspace = root.join("workspace");
        fs::create_dir(&root).expect("create development root");
        fs::create_dir(&workspace).expect("create workspace");
        fs::write(workspace.join("source.txt"), ORIGINAL).expect("write source");
        fs::set_permissions(
            workspace.join("source.txt"),
            fs::Permissions::from_mode(0o640),
        )
        .expect("set source mode");

        let mut control = ControlStore::open(&data).expect("open control store");
        control
            .set_development_root(&root, control.revision())
            .expect("set development root");
        let registration = control
            .register_workspace(&workspace, control.revision())
            .expect("register workspace");
        control
            .set_permissions(
                registration.id(),
                PermissionSet {
                    read: true,
                    write: true,
                    ..PermissionSet::default()
                },
                control.revision(),
            )
            .expect("enable read and write");

        Self {
            data,
            workspace,
            workspace_id: registration.id(),
            session_id: Uuid::new_v4(),
            control,
            _temporary: temporary,
        }
    }

    fn binding(&self) -> SessionBinding {
        self.control
            .bind_session(self.session_id, self.workspace_id)
            .expect("bind session")
    }

    fn target(&self) -> FileTarget {
        FileTarget {
            session_id: self.session_id,
            workspace_id: self.workspace_id,
            path: PathBuf::from("source.txt"),
        }
    }

    fn apply(
        &mut self,
        binding: &SessionBinding,
        hook: impl FnOnce(&mut Store),
    ) -> Result<ReplacementReceipt, FileError> {
        let target = self.target();
        FileBroker::new(&mut self.control).apply_with_preparation_hook(
            binding,
            &target,
            &artifact(ORIGINAL),
            PROPOSED,
            hook,
        )
    }

    fn inspect(&mut self) -> Result<CheckpointInspection, FileError> {
        FileBroker::new(&mut self.control).checkpoint_inspect()
    }

    fn operation_dir(&self, id: Uuid) -> PathBuf {
        self.data.join("replacement-v1").join(id.to_string())
    }
}

fn artifact(bytes: &[u8]) -> RecordedArtifact {
    RecordedArtifact {
        bytes: u64::try_from(bytes.len()).expect("bounded test bytes"),
        sha256: Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    }
}

fn uncertain_id(result: Result<ReplacementReceipt, FileError>) -> Uuid {
    match result {
        Err(FileError::Checkpoint(CheckpointError::Uncertain {
            operation_id: Some(id),
            ..
        })) => id,
        other => panic!("expected an uncertain replacement with an operation ID: {other:?}"),
    }
}

fn operation(
    inspection: &CheckpointInspection,
    id: Uuid,
) -> &crate::replacement::RecordedReplacement {
    inspection
        .operations
        .iter()
        .find(|operation| operation.operation_id == id)
        .expect("operation retained in inspection")
}

fn assert_payloads(fixture: &Fixture, id: Uuid) {
    let directory = fixture.operation_dir(id);
    assert_eq!(fs::read(directory.join("original")).unwrap(), ORIGINAL);
    assert_eq!(fs::read(directory.join("proposed")).unwrap(), PROPOSED);
}

fn assert_stage(fixture: &Fixture, id: Uuid, expected: bool) {
    let path = fixture.operation_dir(id).join("stage");
    assert_eq!(path.exists(), expected, "stage presence: {path:?}");
    if expected {
        assert_eq!(fs::read(path).expect("read retained stage"), PROPOSED);
    }
}

#[test]
fn independent_edits_after_checkpoint_conflict_and_preserve_evidence() {
    for replace_inode in [false, true] {
        let mut fixture = Fixture::new();
        let binding = fixture.binding();
        let target = fixture.workspace.join("source.txt");
        let workspace = fixture.workspace.clone();
        let independent = if replace_inode {
            ORIGINAL
        } else {
            b"independent in-place edit".as_slice()
        };
        let result = fixture.apply(&binding, |_store| {
            if replace_inode {
                let replacement = workspace.join("independent.txt");
                fs::write(&replacement, independent).expect("write independent inode");
                fs::set_permissions(&replacement, fs::Permissions::from_mode(0o640))
                    .expect("set replacement mode");
                fs::rename(replacement, &target).expect("replace target inode");
            } else {
                fs::write(&target, independent).expect("edit target in place");
            }
        });
        assert!(
            matches!(
                result,
                Err(FileError::Checkpoint(CheckpointError::Conflict))
            ),
            "expected conflict for inode replacement={replace_inode}, got {result:?}"
        );
        assert_eq!(fs::read(&target).unwrap(), independent);
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );

        let inspection = fixture.inspect().expect("inspect conflict evidence");
        assert_eq!(inspection.pending, None);
        let record = inspection
            .operations
            .iter()
            .find(|record| record.progress == ReplacementProgress::StagePrepared)
            .expect("stage-prepared conflict record");
        assert_eq!(record.outcome, Some(ReplacementOutcome::Conflict));
        assert_stage(&fixture, record.operation_id, true);
        assert_payloads(&fixture, record.operation_id);
    }
}

#[test]
fn publication_faults_reopen_with_exact_progress_and_no_automatic_retry() {
    let cases = [
        (
            "stage_prepared_sync",
            ReplacementProgress::StagePrepared,
            false,
            None,
        ),
        (
            "commit_intent_sync",
            ReplacementProgress::CommitIntended,
            false,
            None,
        ),
        (
            "before_native_commit",
            ReplacementProgress::CommitIntended,
            false,
            None,
        ),
        (
            "after_native_commit",
            ReplacementProgress::CommitIntended,
            true,
            None,
        ),
        (
            "publication_outcome_sync",
            ReplacementProgress::CommitIntended,
            true,
            Some(ReplacementOutcome::Succeeded),
        ),
    ];
    for (fault, progress, applied, outcome) in cases {
        let mut fixture = Fixture::new();
        let binding = fixture.binding();
        let id = uncertain_id(fixture.apply(&binding, |store| {
            store.inject_publication_fault(fault);
        }));
        assert_eq!(
            fs::read(fixture.workspace.join("source.txt")).unwrap(),
            if applied { PROPOSED } else { ORIGINAL }
        );
        assert_eq!(
            fs::metadata(fixture.workspace.join("source.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert_stage(&fixture, id, !applied);
        assert_payloads(&fixture, id);

        let inspection = fixture.inspect().expect("reopen exact ledger evidence");
        assert_eq!(inspection.pending, outcome.is_none().then_some(id));
        let record = operation(&inspection, id);
        assert_eq!(record.progress, progress);
        assert_eq!(record.outcome, outcome);

        if fault == "after_native_commit" {
            FileBroker::new(&mut fixture.control)
                .checkpoint_acknowledge(id)
                .expect("acknowledge without claiming success");
            let acknowledged = fixture.inspect().expect("inspect acknowledgement");
            let record = operation(&acknowledged, id);
            assert!(record.acknowledged);
            assert_eq!(record.outcome, None);
            assert_eq!(acknowledged.pending, None);
            assert_eq!(
                fs::read(fixture.workspace.join("source.txt")).unwrap(),
                PROPOSED
            );
        }
    }
}

#[test]
fn partial_publication_records_fail_closed_without_changing_evidence() {
    for (fault, applied) in [
        ("stage_prepared_partial_write", false),
        ("commit_intent_partial_write", false),
        ("publication_outcome_partial_write", true),
    ] {
        let mut fixture = Fixture::new();
        let binding = fixture.binding();
        let id = uncertain_id(fixture.apply(&binding, |store| {
            store.inject_publication_fault(fault);
        }));
        let target = fixture.workspace.join("source.txt");
        assert_eq!(
            fs::read(&target).unwrap(),
            if applied { PROPOSED } else { ORIGINAL }
        );
        assert_stage(&fixture, id, !applied);
        assert_payloads(&fixture, id);

        let ledger = fixture.data.join("replacement-v1").join("ledger.jsonl");
        let before = fs::read(&ledger).expect("read partial ledger evidence");
        assert!(fixture.inspect().is_err(), "{fault} must not replay");
        assert!(
            FileBroker::new(&mut fixture.control)
                .checkpoint_acknowledge(id)
                .is_err(),
            "{fault} must not acknowledge corrupt evidence"
        );
        assert_eq!(fs::read(&ledger).expect("ledger preserved"), before);
        assert_eq!(
            fs::read(&target).expect("target preserved"),
            if applied { PROPOSED } else { ORIGINAL }
        );
        assert_payloads(&fixture, id);
    }
}

#[test]
fn tampered_stage_metadata_fails_closed_and_preserves_the_bytes() {
    let mut fixture = Fixture::new();
    let binding = fixture.binding();
    let id = uncertain_id(fixture.apply(&binding, |store| {
        store.inject_publication_fault("before_native_commit");
    }));
    let stage = fixture.operation_dir(id).join("stage");
    let stage_bytes = fs::read(&stage).expect("read retained stage");
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o600)).expect("tamper stage mode");
    let mode = fs::metadata(&stage).unwrap().permissions().mode() & 0o777;
    let ledger = fixture.data.join("replacement-v1").join("ledger.jsonl");
    let ledger_bytes = fs::read(&ledger).expect("read complete ledger");

    assert!(
        fixture.inspect().is_err(),
        "stage metadata tampering must fail closed"
    );
    assert!(
        FileBroker::new(&mut fixture.control)
            .checkpoint_acknowledge(id)
            .is_err(),
        "tampered stage cannot be acknowledged"
    );
    assert_eq!(fs::read(&stage).expect("stage preserved"), stage_bytes);
    assert_eq!(
        fs::metadata(&stage).unwrap().permissions().mode() & 0o777,
        mode
    );
    assert_eq!(fs::read(&ledger).expect("ledger preserved"), ledger_bytes);
    assert_payloads(&fixture, id);
    assert_eq!(
        fs::read(fixture.workspace.join("source.txt")).unwrap(),
        ORIGINAL
    );
}

#[test]
fn apply_rejects_foreign_and_stale_bindings_and_either_denied_preference() {
    let mut fixture = Fixture::new();
    let live = fixture.binding();
    let revision = fixture.control.revision();
    let mismatches = [
        SessionBinding::new(
            Uuid::new_v4(),
            fixture.session_id,
            fixture.workspace_id,
            revision,
        ),
        SessionBinding::new(
            live.store_id(),
            Uuid::new_v4(),
            fixture.workspace_id,
            revision,
        ),
        SessionBinding::new(
            live.store_id(),
            fixture.session_id,
            WorkspaceId::fresh(),
            revision,
        ),
    ];
    for binding in mismatches {
        assert!(matches!(
            fixture.apply(&binding, |_| {}),
            Err(FileError::Checkpoint(CheckpointError::Denied(_)))
        ));
    }
    for (read, write) in [(false, true), (true, false)] {
        let stale = fixture.binding();
        fixture
            .control
            .set_permissions(
                fixture.workspace_id,
                PermissionSet {
                    read,
                    write,
                    ..PermissionSet::default()
                },
                fixture.control.revision(),
            )
            .expect("change current preferences");
        let fresh = fixture.binding();
        for binding in [&stale, &fresh] {
            assert!(matches!(
                fixture.apply(binding, |_| {}),
                Err(FileError::Checkpoint(CheckpointError::Denied(_)))
            ));
        }
    }
    assert_eq!(
        fs::read(fixture.workspace.join("source.txt")).expect("unchanged source"),
        ORIGINAL
    );
    assert!(!fixture.data.join("replacement-init-v1.jsonl").exists());
    assert!(!fixture.data.join("replacement-v1").exists());
    assert!(!fixture.data.join("file-operations-v1.jsonl").exists());
}
