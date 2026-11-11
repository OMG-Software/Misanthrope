use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

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
        set_source_security(&workspace.join("source.txt"));
        #[cfg(windows)]
        make_private_root(&data);

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

/// Unix fixtures use a distinct mode. Windows sources keep their inherited DACL.
fn set_source_security(path: &Path) {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o640)).expect("set source mode");
    #[cfg(windows)]
    let _ = path;
}

/// Reads the security that a replacement must preserve, as comparable text.
fn source_security(path: &Path) -> String {
    #[cfg(unix)]
    {
        format!(
            "{:o}",
            fs::metadata(path).unwrap().permissions().mode() & 0o777
        )
    }
    #[cfg(windows)]
    {
        icacls(&[path.as_os_str()])
    }
}

/// Runs `icacls` and returns its output with the path line normalized away.
#[cfg(windows)]
fn icacls(arguments: &[&std::ffi::OsStr]) -> String {
    let output = std::process::Command::new("icacls")
        .args(arguments)
        .output()
        .expect("icacls");
    assert!(output.status.success(), "icacls fixture failed");
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let path = arguments[0].to_string_lossy().into_owned();
    text.replace(&path, "")
}

/// Gives the data root the private ACL the Windows store requires.
#[cfg(windows)]
fn make_private_root(data: &Path) {
    let sid = current_user_sid();
    fs::create_dir(data).expect("application data");
    icacls(&[
        data.as_os_str(),
        "/inheritance:r".as_ref(),
        "/grant:r".as_ref(),
        format!("*{sid}:(OI)(CI)F").as_ref(),
    ]);
}

#[cfg(windows)]
fn current_user_sid() -> String {
    let output = std::process::Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .expect("whoami");
    assert!(output.status.success(), "whoami fixture failed");
    let identity = String::from_utf8_lossy(&output.stdout).into_owned();
    identity
        .trim()
        .rsplit(',')
        .next()
        .expect("SID column")
        .trim_matches('"')
        .to_owned()
}

/// Gives `path` an explicit protected DACL: full access for the current user
/// and read access for BUILTIN\Users.
#[cfg(windows)]
fn grant_users_read(path: &Path) {
    let sid = current_user_sid();
    icacls(&[
        path.as_os_str(),
        "/inheritance:r".as_ref(),
        "/grant:r".as_ref(),
        format!("*{sid}:F").as_ref(),
        "*S-1-5-32-545:R".as_ref(),
    ]);
}

/// Reports whether `path` grants BUILTIN\Users access.
#[cfg(windows)]
fn grants_users(path: &Path) -> bool {
    let output = std::process::Command::new("icacls")
        .arg(path)
        .args(["/findsid", "*S-1-5-32-545"])
        .output()
        .expect("icacls");
    assert!(output.status.success(), "icacls fixture failed");
    String::from_utf8_lossy(&output.stdout).contains(&*path.to_string_lossy())
}

/// Copies the owner, group, and protected DACL of `from` onto `to`, as an
/// interrupted commit attempt would leave a stage.
#[cfg(windows)]
fn copy_security(from: &Path, to: &Path) {
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            // .NET only, no cmdlets: a module path inherited from another shell
            // can stop Windows PowerShell loading them. Set-Acl would also
            // write the audit SACL, which needs elevation.
            "$parts = [System.Security.AccessControl.AccessControlSections]'Owner,Group,Access'; \
             $sddl = [System.IO.File]::GetAccessControl($env:OMG_FROM).GetSecurityDescriptorSddlForm($parts); \
             $copy = [System.Security.AccessControl.FileSecurity]::new(); \
             $copy.SetSecurityDescriptorSddlForm($sddl, $parts); \
             [System.IO.File]::SetAccessControl($env:OMG_TO, $copy)",
        ])
        .env_remove("PSModulePath")
        .env("OMG_FROM", from)
        .env("OMG_TO", to)
        .output()
        .expect("powershell");
    assert!(
        output.status.success() && output.stderr.is_empty(),
        "security copy fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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

/// An edit cannot reach a Windows target in place while its source handle
/// denies write sharing, so only the entry swap is a possible conflict there.
fn independent_edit_cases() -> &'static [bool] {
    if cfg!(windows) {
        &[true]
    } else {
        &[false, true]
    }
}

#[test]
fn independent_edits_after_checkpoint_conflict_and_preserve_evidence() {
    for &replace_inode in independent_edit_cases() {
        let mut fixture = Fixture::new();
        let binding = fixture.binding();
        let target = fixture.workspace.join("source.txt");
        let workspace = fixture.workspace.clone();
        let security = source_security(&target);
        let independent = if replace_inode {
            ORIGINAL
        } else {
            b"independent in-place edit".as_slice()
        };
        let result = fixture.apply(&binding, |_store| {
            if replace_inode {
                let replacement = workspace.join("independent.txt");
                fs::write(&replacement, independent).expect("write independent inode");
                set_source_security(&replacement);
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
        assert_eq!(source_security(&target), security);

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
        let security = source_security(&fixture.workspace.join("source.txt"));
        let id = uncertain_id(fixture.apply(&binding, |store| {
            store.inject_publication_fault(fault);
        }));
        assert_eq!(
            fs::read(fixture.workspace.join("source.txt")).unwrap(),
            if applied { PROPOSED } else { ORIGINAL }
        );
        assert_eq!(
            source_security(&fixture.workspace.join("source.txt")),
            security
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

/// Change the stage's security so it no longer matches its recorded source.
fn tamper_stage_security(stage: &Path) {
    #[cfg(unix)]
    fs::set_permissions(stage, fs::Permissions::from_mode(0o600)).expect("tamper stage mode");
    #[cfg(windows)]
    icacls(&[
        stage.as_os_str(),
        "/grant".as_ref(),
        "*S-1-1-0:(R)".as_ref(),
    ]);
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
    tamper_stage_security(&stage);
    let security = source_security(&stage);
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
    assert_eq!(source_security(&stage), security);
    assert_eq!(fs::read(&ledger).expect("ledger preserved"), ledger_bytes);
    assert_payloads(&fixture, id);
    assert_eq!(
        fs::read(fixture.workspace.join("source.txt")).unwrap(),
        ORIGINAL
    );
}

/// A crash after the stage takes the source security, but before the rename,
/// leaves that security on a retained stage. Recovery accepts it only after
/// commit intent.
#[cfg(windows)]
#[test]
fn an_interrupted_attempt_with_the_source_security_reopens_after_commit_intent() {
    let mut fixture = Fixture::new();
    let binding = fixture.binding();
    let source = fixture.workspace.join("source.txt");
    let twin = fixture.workspace.join("twin.txt");
    fs::write(&twin, b"twin").expect("twin");
    grant_users_read(&source);
    grant_users_read(&twin);
    let id = uncertain_id(fixture.apply(&binding, |store| {
        store.inject_publication_fault("before_native_commit");
    }));
    let stage = fixture.operation_dir(id).join("stage");
    assert!(
        !grants_users(&stage),
        "the stage is private before the attempt"
    );
    copy_security(&twin, &stage);
    assert!(grants_users(&stage), "fixture recreates the crash state");

    let inspection = fixture
        .inspect()
        .expect("an interrupted attempt is accepted after commit intent");
    assert_eq!(
        operation(&inspection, id).progress,
        ReplacementProgress::CommitIntended
    );
    assert_eq!(fs::read(&stage).expect("stage preserved"), PROPOSED);
    assert_eq!(fs::read(&source).expect("target unchanged"), ORIGINAL);
}

/// Before commit intent the stage must still be private, even when its
/// security exactly matches the source.
#[cfg(windows)]
#[test]
fn a_conflict_stage_with_the_source_security_fails_closed() {
    let mut fixture = Fixture::new();
    let binding = fixture.binding();
    let target = fixture.workspace.join("source.txt");
    let twin = fixture.workspace.join("twin.txt");
    fs::write(&twin, b"twin").expect("twin");
    grant_users_read(&target);
    grant_users_read(&twin);
    let workspace = fixture.workspace.clone();
    let result = fixture.apply(&binding, |_store| {
        let replacement = workspace.join("independent.txt");
        fs::write(&replacement, ORIGINAL).expect("write independent file");
        fs::rename(replacement, workspace.join("source.txt")).expect("replace target");
    });
    assert!(matches!(
        result,
        Err(FileError::Checkpoint(CheckpointError::Conflict))
    ));
    let inspection = fixture.inspect().expect("inspect conflict evidence");
    let id = inspection
        .operations
        .iter()
        .find(|record| record.progress == ReplacementProgress::StagePrepared)
        .expect("stage-prepared conflict record")
        .operation_id;
    let stage = fixture.operation_dir(id).join("stage");
    assert!(!grants_users(&stage), "a conflict retains a private stage");
    copy_security(&twin, &stage);
    assert!(
        fixture.inspect().is_err(),
        "a non-private stage before commit intent fails closed"
    );
    assert_eq!(fs::read(&stage).expect("stage preserved"), PROPOSED);
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
