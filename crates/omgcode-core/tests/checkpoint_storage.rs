use std::fs;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;

use omgcode_core::control::ControlStore;
use omgcode_core::files::{FileBroker, FileError, FileTarget};
#[cfg(target_os = "linux")]
use omgcode_core::policy::DecisionReason;
#[cfg(target_os = "linux")]
use omgcode_core::policy::PermissionSet;
use omgcode_core::policy::{SessionBinding, WorkspaceId};
use omgcode_core::replacement::RecordedArtifact;
#[cfg(target_os = "linux")]
use omgcode_core::replacement::ReplacementProgress;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use uuid::Uuid;

const PREPARED_ROOT: &str = "replacement-v1";
const INITIALIZATION_LOG: &str = "replacement-init-v1.jsonl";
const LEDGER: &str = "replacement-v1/ledger.jsonl";

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
        let temporary = tempfile::tempdir().expect("create private temporary directory");
        let data = temporary.path().join("application-data");
        let root = temporary.path().join("development-root");
        let workspace = root.join("workspace");
        fs::create_dir(&root).expect("create development root");
        fs::create_dir(&workspace).expect("create workspace");

        let mut control = ControlStore::open(&data).expect("open control store");
        control
            .set_development_root(&root, control.revision())
            .expect("set development root");
        let registration = control
            .register_workspace(&workspace, control.revision())
            .expect("register workspace");

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
            .expect("bind workspace session")
    }

    fn replacement_paths(&self) -> [PathBuf; 3] {
        [
            self.data.join(INITIALIZATION_LOG),
            self.data.join(LEDGER),
            self.data.join(PREPARED_ROOT),
        ]
    }
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn target(session_id: Uuid, workspace_id: WorkspaceId, path: impl Into<PathBuf>) -> FileTarget {
    FileTarget {
        session_id,
        workspace_id,
        path: path.into(),
    }
}

#[cfg(target_os = "linux")]
fn assert_artifact(artifact: &RecordedArtifact, bytes: &[u8]) {
    assert_eq!(artifact.bytes, bytes.len() as u64);
    assert_eq!(artifact.sha256, digest(bytes));
}

#[cfg(target_os = "linux")]
fn path_from_native(path: &omgcode_core::session::NativePath) -> PathBuf {
    path.to_path_buf()
        .expect("recorded target uses native host paths")
}

#[cfg(target_os = "linux")]
fn entry_paths(directory: &Path) -> Vec<PathBuf> {
    let mut entries = fs::read_dir(directory)
        .expect("read private directory entries")
        .map(|entry| entry.expect("read private directory entry").path())
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

#[cfg(target_os = "linux")]
fn managed_file_bytes(fixture: &Fixture, expected_operations: usize) -> u64 {
    use std::ffi::OsStr;

    let store_path = fixture.data.join(PREPARED_ROOT);
    let ledger_path = fixture.data.join(LEDGER);
    let anchor_path = fixture.data.join(INITIALIZATION_LOG);
    let mut total = fs::metadata(&ledger_path)
        .expect("stat checkpoint ledger")
        .len()
        + fs::metadata(&anchor_path)
            .expect("stat initialization anchor")
            .len();
    let mut operation_count = 0;

    for entry in entry_paths(&store_path) {
        if entry == ledger_path {
            continue;
        }
        assert!(
            entry.is_dir(),
            "private store entry is not a directory: {entry:?}"
        );
        operation_count += 1;
        for artifact in fs::read_dir(&entry)
            .expect("read checkpoint operation directory")
            .map(|child| child.expect("read checkpoint artifact").path())
        {
            let name = artifact.file_name().expect("artifact has a filename");
            assert!(
                ["original", "proposed", "manifest.json", "stage"]
                    .iter()
                    .any(|known| name == OsStr::new(known)),
                "unknown checkpoint artifact in measured fixture: {artifact:?}"
            );
            let metadata = fs::symlink_metadata(&artifact).expect("stat checkpoint artifact");
            assert!(
                metadata.file_type().is_file(),
                "checkpoint artifact is not a regular file: {artifact:?}"
            );
            total += metadata.len();
        }
    }
    assert_eq!(operation_count, expected_operations);
    total
}

fn read_bytes_are_unchanged(fixture: &mut Fixture, binding: &SessionBinding, expected: &[u8]) {
    let target = FileTarget {
        session_id: fixture.session_id,
        workspace_id: fixture.workspace_id,
        path: PathBuf::from("source.bin"),
    };
    let read = FileBroker::new(&mut fixture.control)
        .read(binding, &target, 1024)
        .expect("read source file");
    assert_eq!(read.bytes, expected);
}

#[test]
fn existing_reads_do_not_initialize_replacement_storage() {
    let mut fixture = Fixture::new();
    let source = b"unchanged source bytes";
    fs::write(fixture.workspace.join("source.bin"), source).expect("write source file");
    let binding = fixture.binding();

    let inspection = FileBroker::new(&mut fixture.control)
        .checkpoint_inspect()
        .expect("inspect unused replacement storage");
    assert_eq!(inspection.state, "unused");
    assert_eq!(inspection.managed_bytes, 0);
    assert!(inspection.operations.is_empty());
    read_bytes_are_unchanged(&mut fixture, &binding, source);

    for path in fixture.replacement_paths() {
        assert!(!path.exists(), "unused replacement path exists: {path:?}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn prepare_retains_exact_private_payload_and_acknowledgement_does_not_apply_it() {
    use std::os::unix::fs::PermissionsExt;

    let mut fixture = Fixture::new();
    let original = b"original\0native bytes\n";
    let proposed = b"proposed\0replacement bytes\r\n";
    fs::write(fixture.workspace.join("source.bin"), original).expect("write source file");
    let binding = fixture.binding();

    let receipt = FileBroker::new(&mut fixture.control)
        .checkpoint_prepare(
            &binding,
            &target(fixture.session_id, fixture.workspace_id, "source.bin"),
            &RecordedArtifact {
                bytes: original.len() as u64,
                sha256: digest(original),
            },
            proposed,
        )
        .expect("prepare checkpoint");
    assert!(!receipt.applied);
    assert_artifact(&receipt.original, original);
    assert_artifact(&receipt.proposed, proposed);
    assert_eq!(
        fs::read(fixture.workspace.join("source.bin")).unwrap(),
        original
    );

    let operation_dir = fixture
        .data
        .join(PREPARED_ROOT)
        .join(receipt.operation_id.to_string());
    let original_path = operation_dir.join("original");
    let proposed_path = operation_dir.join("proposed");
    let manifest_path = operation_dir.join("manifest.json");
    assert_eq!(
        fs::read(&original_path).expect("read retained original"),
        original
    );
    assert_eq!(
        fs::read(&proposed_path).expect("read retained proposal"),
        proposed
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("read strict manifest"))
            .expect("parse manifest");
    assert!(manifest.is_object());
    assert_eq!(manifest["original"]["bytes"], original.len() as u64);
    assert_eq!(manifest["original"]["sha256"], digest(original));
    assert_eq!(manifest["proposed"]["bytes"], proposed.len() as u64);
    assert_eq!(manifest["proposed"]["sha256"], digest(proposed));

    assert_eq!(
        fs::metadata(&fixture.data).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(fixture.data.join(PREPARED_ROOT))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&operation_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for path in [&original_path, &proposed_path, &manifest_path] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    let legacy_log = fixture.data.join("file-operations-v1.jsonl");
    let before_blocked_calls = fs::read(&legacy_log).expect("read legacy operation log");
    let blocked_read = FileBroker::new(&mut fixture.control)
        .read(
            &binding,
            &target(fixture.session_id, fixture.workspace_id, "source.bin"),
            1024,
        )
        .expect_err("pending checkpoint blocks reads");
    assert!(blocked_read.to_string().contains("unresolved"));
    let blocked_create = FileBroker::new(&mut fixture.control)
        .create_file(
            &binding,
            &target(fixture.session_id, fixture.workspace_id, "new.bin"),
            b"new",
        )
        .expect_err("pending checkpoint blocks creates");
    assert!(blocked_create.to_string().contains("unresolved"));
    assert_eq!(fs::read(&legacy_log).unwrap(), before_blocked_calls);
    assert!(!fixture.workspace.join("new.bin").exists());

    drop(fixture.control);
    fixture.control = ControlStore::open(&fixture.data).expect("reopen control store");
    let ledger_path = fixture.data.join(LEDGER);
    let anchor_path = fixture.data.join(INITIALIZATION_LOG);
    let ledger_before_inspection = fs::read(&ledger_path).expect("read ledger before inspection");
    let anchor_before_inspection = fs::read(&anchor_path).expect("read anchor before inspection");
    let inspection = FileBroker::new(&mut fixture.control)
        .checkpoint_inspect()
        .expect("inspect pending checkpoint after reopen");
    assert_eq!(fs::read(&ledger_path).unwrap(), ledger_before_inspection);
    assert_eq!(fs::read(&anchor_path).unwrap(), anchor_before_inspection);
    assert_eq!(inspection.pending, Some(receipt.operation_id));
    let operation = inspection
        .operations
        .iter()
        .find(|operation| operation.operation_id == receipt.operation_id)
        .expect("find recorded replacement");
    assert_eq!(operation.progress, ReplacementProgress::CheckpointPrepared);
    assert_eq!(path_from_native(&operation.target), Path::new("source.bin"));
    assert_artifact(&operation.expected, original);
    assert_artifact(&operation.proposed, proposed);
    assert!(inspection.managed_bytes > 0);
    let inspection_json = serde_json::to_string(&inspection).expect("serialize metadata view");
    assert!(!inspection_json.contains("native bytes"));
    assert!(!inspection_json.contains("replacement bytes"));

    FileBroker::new(&mut fixture.control)
        .checkpoint_acknowledge(receipt.operation_id)
        .expect("acknowledge recorded blocker");
    assert_eq!(
        fs::read(fixture.workspace.join("source.bin")).unwrap(),
        original
    );
    read_bytes_are_unchanged(&mut fixture, &binding, original);
    assert!(
        FileBroker::new(&mut fixture.control)
            .checkpoint_acknowledge(receipt.operation_id)
            .is_err()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn wrong_acknowledgement_id_does_not_create_or_clear_checkpoint_state() {
    let mut fixture = Fixture::new();
    let binding = fixture.binding();
    let wrong_id = Uuid::new_v4();

    assert!(
        FileBroker::new(&mut fixture.control)
            .checkpoint_acknowledge(wrong_id)
            .is_err()
    );
    for path in fixture.replacement_paths() {
        assert!(!path.exists(), "wrong acknowledgement created {path:?}");
    }
    assert!(
        FileBroker::new(&mut fixture.control)
            .checkpoint_inspect()
            .expect("inspect unused storage")
            .operations
            .is_empty()
    );
    fs::write(fixture.workspace.join("source.bin"), b"readable").unwrap();
    read_bytes_are_unchanged(&mut fixture, &binding, b"readable");
}

#[cfg(target_os = "linux")]
#[test]
fn policy_denials_and_invalid_or_stale_inputs_do_not_initialize_storage() {
    let mut denied = Fixture::new();
    fs::write(denied.workspace.join("source.bin"), b"original").unwrap();
    denied
        .control
        .set_permissions(
            denied.workspace_id,
            PermissionSet {
                read: false,
                ..PermissionSet::default()
            },
            denied.control.revision(),
        )
        .expect("disable reads");
    let denied_binding = denied.binding();
    assert!(
        FileBroker::new(&mut denied.control)
            .checkpoint_prepare(
                &denied_binding,
                &target(denied.session_id, denied.workspace_id, "source.bin"),
                &RecordedArtifact {
                    bytes: 8,
                    sha256: digest(b"original"),
                },
                b"proposal",
            )
            .is_err()
    );
    for path in denied.replacement_paths() {
        assert!(!path.exists(), "permission denial created {path:?}");
    }

    let mut write_denied = Fixture::new();
    fs::write(write_denied.workspace.join("source.bin"), b"original").unwrap();
    write_denied
        .control
        .set_permissions(
            write_denied.workspace_id,
            PermissionSet {
                write: false,
                ..PermissionSet::default()
            },
            write_denied.control.revision(),
        )
        .expect("disable writes");
    let write_binding = write_denied.binding();
    let write_denied_error = FileBroker::new(&mut write_denied.control)
        .checkpoint_prepare(
            &write_binding,
            &target(
                write_denied.session_id,
                write_denied.workspace_id,
                "source.bin",
            ),
            &RecordedArtifact {
                bytes: 8,
                sha256: digest(b"original"),
            },
            b"proposal",
        )
        .expect_err("checkpoint preparation requires write permission");
    assert!(
        matches!(write_denied_error, FileError::Checkpoint(error) if error.code() == "checkpoint_denied")
    );
    for path in write_denied.replacement_paths() {
        assert!(!path.exists(), "write denial created {path:?}");
    }

    for expected in [
        RecordedArtifact {
            bytes: 8,
            sha256: digest(b"original!"),
        },
        RecordedArtifact {
            bytes: 9,
            sha256: digest(b"original"),
        },
    ] {
        let mut stale = Fixture::new();
        fs::write(stale.workspace.join("source.bin"), b"original").unwrap();
        let binding = stale.binding();
        let stale_error = FileBroker::new(&mut stale.control)
            .checkpoint_prepare(
                &binding,
                &target(stale.session_id, stale.workspace_id, "source.bin"),
                &expected,
                b"proposal",
            )
            .expect_err("stale expected size or digest must conflict");
        assert!(
            matches!(stale_error, FileError::Checkpoint(error) if error.code() == "checkpoint_conflict")
        );
        assert_eq!(
            fs::read(stale.workspace.join("source.bin")).unwrap(),
            b"original"
        );
        for path in stale.replacement_paths() {
            assert!(!path.exists(), "stale input created {path:?}");
        }
    }

    for invalid in [&[0xff_u8][..], &vec![b'x'; 1024 * 1024 + 1][..]] {
        let mut invalid_fixture = Fixture::new();
        fs::write(invalid_fixture.workspace.join("source.bin"), b"original").unwrap();
        let invalid_binding = invalid_fixture.binding();
        assert!(
            FileBroker::new(&mut invalid_fixture.control)
                .checkpoint_prepare(
                    &invalid_binding,
                    &target(
                        invalid_fixture.session_id,
                        invalid_fixture.workspace_id,
                        "source.bin"
                    ),
                    &RecordedArtifact {
                        bytes: 8,
                        sha256: digest(b"original"),
                    },
                    invalid,
                )
                .is_err()
        );
        for path in invalid_fixture.replacement_paths() {
            assert!(!path.exists(), "invalid input created {path:?}");
        }
    }

    let mut invalid_source = Fixture::new();
    let source = [0xff_u8];
    fs::write(invalid_source.workspace.join("source.bin"), source).unwrap();
    let invalid_source_binding = invalid_source.binding();
    let invalid_source_error = FileBroker::new(&mut invalid_source.control)
        .checkpoint_prepare(
            &invalid_source_binding,
            &target(
                invalid_source.session_id,
                invalid_source.workspace_id,
                "source.bin",
            ),
            &RecordedArtifact {
                bytes: source.len() as u64,
                sha256: digest(&source),
            },
            b"valid proposal",
        )
        .expect_err("non-UTF-8 source is refused");
    assert!(matches!(invalid_source_error, FileError::Invalid(_)));
    assert_eq!(
        fs::read(invalid_source.workspace.join("source.bin")).unwrap(),
        source
    );
    for path in invalid_source.replacement_paths() {
        assert!(!path.exists(), "invalid source created {path:?}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn links_are_refused_without_creating_checkpoint_artifacts() {
    use std::os::unix::fs::symlink;

    let mut fixture = Fixture::new();
    let original = b"single linked regular source";
    fs::write(fixture.workspace.join("source.bin"), original).unwrap();
    symlink("source.bin", fixture.workspace.join("symbolic.bin")).unwrap();
    fs::hard_link(
        fixture.workspace.join("source.bin"),
        fixture.workspace.join("hard.bin"),
    )
    .unwrap();
    let binding = fixture.binding();
    for name in ["symbolic.bin", "hard.bin"] {
        assert!(
            FileBroker::new(&mut fixture.control)
                .checkpoint_prepare(
                    &binding,
                    &target(fixture.session_id, fixture.workspace_id, name),
                    &RecordedArtifact {
                        bytes: original.len() as u64,
                        sha256: digest(original),
                    },
                    b"proposal",
                )
                .is_err()
        );
    }
    for path in fixture.replacement_paths() {
        assert!(!path.exists(), "link refusal created {path:?}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn corrupt_or_missing_payload_blocks_operations_and_is_never_recreated() {
    for corrupt in [false, true] {
        let mut fixture = Fixture::new();
        let original = b"retained source";
        fs::write(fixture.workspace.join("source.bin"), original).unwrap();
        let binding = fixture.binding();
        let receipt = FileBroker::new(&mut fixture.control)
            .checkpoint_prepare(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, "source.bin"),
                &RecordedArtifact {
                    bytes: original.len() as u64,
                    sha256: digest(original),
                },
                b"proposed source",
            )
            .expect("prepare checkpoint");
        let proposed_path = fixture
            .data
            .join(PREPARED_ROOT)
            .join(receipt.operation_id.to_string())
            .join("proposed");
        if corrupt {
            fs::write(&proposed_path, b"tampered payload").expect("corrupt retained bytes");
        } else {
            fs::remove_file(&proposed_path).expect("remove retained proposed bytes");
        }

        assert!(
            FileBroker::new(&mut fixture.control)
                .checkpoint_inspect()
                .is_err()
        );
        assert!(
            FileBroker::new(&mut fixture.control)
                .checkpoint_prepare(
                    &binding,
                    &target(fixture.session_id, fixture.workspace_id, "source.bin"),
                    &RecordedArtifact {
                        bytes: original.len() as u64,
                        sha256: digest(original),
                    },
                    b"another proposal",
                )
                .is_err()
        );
        assert_eq!(
            fs::read(&proposed_path).ok(),
            corrupt.then(|| b"tampered payload".to_vec())
        );
        assert_eq!(
            fs::read(fixture.workspace.join("source.bin")).unwrap(),
            original
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn unsafe_private_directory_file_mode_and_unknown_entry_fail_closed() {
    use std::os::unix::fs::PermissionsExt;

    for tamper in ["directory-mode", "file-mode", "unknown-entry"] {
        let mut fixture = Fixture::new();
        let original = b"original source";
        fs::write(fixture.workspace.join("source.bin"), original).unwrap();
        let binding = fixture.binding();
        let receipt = FileBroker::new(&mut fixture.control)
            .checkpoint_prepare(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, "source.bin"),
                &RecordedArtifact {
                    bytes: original.len() as u64,
                    sha256: digest(original),
                },
                b"proposed source",
            )
            .expect("prepare checkpoint");
        let store_path = fixture.data.join(PREPARED_ROOT);
        let operation_dir = store_path.join(receipt.operation_id.to_string());
        let tampered_file = operation_dir.join("proposed");
        match tamper {
            "directory-mode" => fs::set_permissions(&store_path, fs::Permissions::from_mode(0o755))
                .expect("relax private directory mode"),
            "file-mode" => fs::set_permissions(&tampered_file, fs::Permissions::from_mode(0o644))
                .expect("relax private file mode"),
            "unknown-entry" => fs::write(store_path.join("unexpected"), b"unexpected")
                .expect("add unknown store entry"),
            _ => unreachable!("tamper cases are fixed in this test"),
        }
        let ledger_path = fixture.data.join(LEDGER);
        let ledger_before = fs::read(&ledger_path).expect("read ledger before refusal");

        assert!(
            FileBroker::new(&mut fixture.control)
                .checkpoint_inspect()
                .is_err(),
            "unsafe {tamper} must refuse inspection"
        );
        assert!(
            FileBroker::new(&mut fixture.control)
                .read(
                    &binding,
                    &target(fixture.session_id, fixture.workspace_id, "source.bin"),
                    1024,
                )
                .is_err(),
            "unsafe {tamper} must block normal broker reads"
        );
        assert!(
            FileBroker::new(&mut fixture.control)
                .checkpoint_prepare(
                    &binding,
                    &target(fixture.session_id, fixture.workspace_id, "source.bin"),
                    &RecordedArtifact {
                        bytes: original.len() as u64,
                        sha256: digest(original),
                    },
                    b"another proposal",
                )
                .is_err(),
            "unsafe {tamper} must block new preparations"
        );
        assert_eq!(fs::read(&ledger_path).unwrap(), ledger_before);
        match tamper {
            "directory-mode" => assert_eq!(
                fs::metadata(&store_path).unwrap().permissions().mode() & 0o777,
                0o755
            ),
            "file-mode" => assert_eq!(
                fs::metadata(&tampered_file).unwrap().permissions().mode() & 0o777,
                0o644
            ),
            "unknown-entry" => assert!(store_path.join("unexpected").exists()),
            _ => unreachable!("tamper cases are fixed in this test"),
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn sixty_four_acknowledged_checkpoints_are_retained_and_refuse_the_sixty_fifth() {
    let mut fixture = Fixture::new();
    let original = b"o";
    let proposed = b"p";
    fs::write(fixture.workspace.join("source.bin"), original).unwrap();
    let binding = fixture.binding();
    let expected = RecordedArtifact {
        bytes: original.len() as u64,
        sha256: digest(original),
    };

    for _ in 0..64 {
        let receipt = FileBroker::new(&mut fixture.control)
            .checkpoint_prepare(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, "source.bin"),
                &expected,
                proposed,
            )
            .expect("prepare one retained small checkpoint");
        FileBroker::new(&mut fixture.control)
            .checkpoint_acknowledge(receipt.operation_id)
            .expect("acknowledge retained checkpoint");
    }

    let store_path = fixture.data.join(PREPARED_ROOT);
    let ledger_path = fixture.data.join(LEDGER);
    let ledger_before_refusal = fs::read(&ledger_path).expect("read ledger before limit");
    let entries_before_refusal = entry_paths(&store_path);
    let refusal = FileBroker::new(&mut fixture.control)
        .checkpoint_prepare(
            &binding,
            &target(fixture.session_id, fixture.workspace_id, "source.bin"),
            &expected,
            proposed,
        )
        .expect_err("the 65th operation must be refused");
    assert!(matches!(refusal, FileError::Checkpoint(error) if error.code() == "checkpoint_limit"));
    assert_eq!(fs::read(&ledger_path).unwrap(), ledger_before_refusal);
    assert_eq!(entry_paths(&store_path), entries_before_refusal);

    let inspection = FileBroker::new(&mut fixture.control)
        .checkpoint_inspect()
        .expect("inspect retained operations after refusal");
    assert_eq!(inspection.operations.len(), 64);
    assert_eq!(inspection.pending, None);
    read_bytes_are_unchanged(&mut fixture, &binding, original);

    fixture
        .control
        .set_permissions(
            fixture.workspace_id,
            PermissionSet {
                read: false,
                write: false,
                ..PermissionSet::default()
            },
            fixture.control.revision(),
        )
        .expect("revoke file permissions after storage limit");
    let revoked_binding = fixture.binding();
    let revoked_read = FileBroker::new(&mut fixture.control)
        .read(
            &revoked_binding,
            &target(fixture.session_id, fixture.workspace_id, "source.bin"),
            1024,
        )
        .expect_err("permission revocation remains effective");
    assert!(matches!(
        revoked_read,
        FileError::Denied(DecisionReason::PermissionDisabled)
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn aggregate_checkpoint_storage_refuses_before_exceeding_sixty_four_mib() {
    const PAYLOAD_BYTES: usize = 1024 * 1024;
    const MEBIBYTE: u64 = 1024 * 1024;

    let mut fixture = Fixture::new();
    let original = vec![b'o'; PAYLOAD_BYTES];
    let proposed = vec![b'p'; PAYLOAD_BYTES];
    fs::write(fixture.workspace.join("source.bin"), &original)
        .expect("write full original workspace source");
    let binding = fixture.binding();
    let expected = RecordedArtifact {
        bytes: u64::try_from(original.len()).unwrap(),
        sha256: digest(&original),
    };
    let store_path = fixture.data.join(PREPARED_ROOT);
    let ledger_path = fixture.data.join(LEDGER);
    let mut successes = 0;
    let mut refused = false;

    for _ in 0..34 {
        let ledger_before = ledger_path
            .exists()
            .then(|| fs::read(&ledger_path).expect("snapshot existing ledger before attempt"));
        let entries_before = store_path.exists().then(|| entry_paths(&store_path));
        assert_eq!(ledger_before.is_some(), entries_before.is_some());

        match FileBroker::new(&mut fixture.control).checkpoint_prepare(
            &binding,
            &target(fixture.session_id, fixture.workspace_id, "source.bin"),
            &expected,
            &proposed,
        ) {
            Ok(receipt) => {
                successes += 1;
                FileBroker::new(&mut fixture.control)
                    .checkpoint_acknowledge(receipt.operation_id)
                    .expect("acknowledge full-size checkpoint");
            }
            Err(FileError::Checkpoint(error)) if error.code() == "checkpoint_limit" => {
                let ledger_before =
                    ledger_before.expect("quota refusal requires an existing ledger");
                let entries_before =
                    entries_before.expect("quota refusal requires existing entries");
                assert_eq!(fs::read(&ledger_path).unwrap(), ledger_before);
                assert_eq!(entry_paths(&store_path), entries_before);
                refused = true;
                break;
            }
            Err(error) => panic!("unexpected full-size checkpoint error: {error}"),
        }
    }

    assert!(refused, "quota did not refuse within 34 attempts");
    assert!(
        successes >= 30,
        "expected at least 30 whole checkpoints, got {successes}"
    );
    assert!(successes < 32, "32 full checkpoint pairs must exceed quota");
    assert_eq!(entry_paths(&store_path).len(), successes + 1);

    let inspection = FileBroker::new(&mut fixture.control)
        .checkpoint_inspect()
        .expect("inspect full retained checkpoint store");
    assert_eq!(inspection.operations.len(), successes);
    assert_eq!(inspection.pending, None);
    assert!(inspection.managed_bytes >= 60 * MEBIBYTE);
    assert!(inspection.managed_bytes <= 64 * MEBIBYTE);
    assert_eq!(
        inspection.managed_bytes,
        managed_file_bytes(&fixture, successes),
        "inspection total must equal all actual managed file lengths"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn legacy_world_accessible_data_directory_keeps_no_state_reads_available() {
    use std::os::unix::fs::PermissionsExt;

    let mut fixture = Fixture::new();
    fs::write(fixture.workspace.join("source.bin"), b"existing source").unwrap();
    fs::set_permissions(&fixture.data, fs::Permissions::from_mode(0o755))
        .expect("set legacy application-data permissions");
    let binding = fixture.binding();

    read_bytes_are_unchanged(&mut fixture, &binding, b"existing source");
    let refusal = FileBroker::new(&mut fixture.control)
        .checkpoint_prepare(
            &binding,
            &target(fixture.session_id, fixture.workspace_id, "source.bin"),
            &RecordedArtifact {
                bytes: b"existing source".len() as u64,
                sha256: digest(b"existing source"),
            },
            b"new source bytes",
        )
        .expect_err("legacy world-accessible data root cannot initialize storage");
    assert!(matches!(refusal, FileError::Checkpoint(_)));
    let inspection = FileBroker::new(&mut fixture.control)
        .checkpoint_inspect()
        .expect("inspect unused replacement state");
    assert_eq!(inspection.state, "unused");
    assert!(inspection.operations.is_empty());
    for path in fixture.replacement_paths() {
        assert!(!path.exists(), "legacy no-state read created {path:?}");
    }
}

#[cfg(windows)]
#[test]
fn windows_reports_unused_state_and_rejects_preparation_before_creating_files() {
    let mut fixture = Fixture::new();
    let original = b"existing Windows source";
    fs::write(fixture.workspace.join("source.bin"), original).expect("write source file");
    let binding = fixture.binding();

    let inspection = FileBroker::new(&mut fixture.control)
        .checkpoint_inspect()
        .expect("inspect unused storage");
    assert_eq!(inspection.state, "unused");
    assert!(inspection.operations.is_empty());
    let error = FileBroker::new(&mut fixture.control)
        .checkpoint_prepare(
            &binding,
            &target(fixture.session_id, fixture.workspace_id, "source.bin"),
            &RecordedArtifact {
                bytes: original.len() as u64,
                sha256: digest(original),
            },
            b"proposal",
        )
        .expect_err("Windows checkpoint backend is unsupported");
    assert!(
        matches!(error, FileError::Checkpoint(error) if error.code() == "checkpoint_unsupported")
    );
    for path in fixture.replacement_paths() {
        assert!(!path.exists(), "unsupported preparation created {path:?}");
    }
    read_bytes_are_unchanged(&mut fixture, &binding, original);
}
