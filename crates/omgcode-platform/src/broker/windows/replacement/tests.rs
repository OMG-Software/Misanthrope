use super::*;
use crate::broker::windows::tests::create_junction_bounded;
use crate::private_store::windows::tests::private_root;
use crate::private_store::windows::{apply_security, is_private_file, read_security};
use std::fs::{self, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_HIDDEN, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_WRITE,
    FILE_WRITE_ATTRIBUTES, FileBasicInfo, READ_CONTROL, SetFileInformationByHandle, WRITE_DAC,
    WRITE_OWNER,
};

struct Fixture {
    workspace: NativeWorkspace,
    storage: PathBuf,
    path: PathBuf,
    _root: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("root");
    let workspace_path = root.path().join("workspace");
    fs::create_dir(&workspace_path).expect("workspace");
    let open = |path: &Path| {
        directory_identity(&super::super::open_configured_root(path).expect("directory"))
            .expect("identity")
    };
    let workspace = NativeWorkspace::open(
        root.path(),
        open(root.path()),
        &RelativePath::new(Path::new("workspace")).expect("workspace path"),
        open(&workspace_path),
    )
    .expect("native workspace");
    let storage = private_root(root.path(), "storage");
    Fixture {
        workspace,
        storage,
        path: workspace_path.join("file"),
        _root: root,
    }
}

impl Fixture {
    fn with_source(bytes: &[u8]) -> Self {
        let fixture = fixture();
        fs::write(&fixture.path, bytes).expect("source");
        fixture
    }

    fn target(&self) -> ReplacementTarget<'_> {
        self.target_named("file").expect("target capture")
    }

    fn target_named(&self, name: &str) -> io::Result<ReplacementTarget<'_>> {
        self.workspace
            .replacement_target(&RelativePath::new(Path::new(name)).expect("path"), 1024)
    }

    fn directory(&self) -> PrivateDirectory {
        PrivateDirectory::open(&self.storage).expect("private directory")
    }

    fn stage_path(&self) -> PathBuf {
        self.storage.join(STAGE_NAME)
    }
}

fn descriptor(capture: &FileCapture) -> &str {
    let NativeSecurity::Windows { descriptor, .. } = &capture.security else {
        panic!("Windows capture has Windows security");
    };
    descriptor
}

fn security_handle(path: &Path) -> File {
    OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC | WRITE_OWNER)
        .open(path)
        .expect("security handle")
}

/// Replace only the DACL of a file, keeping its owner and group.
fn set_dacl(path: &Path, dacl: &str) {
    let file = security_handle(path);
    let current = read_security(&file).expect("current security");
    let prefix = current.split("D:").next().expect("owner and group");
    apply_security(&file, &format!("{prefix}D:{dacl}")).expect("set DACL");
}

/// Give a directory explicit inheritable entries, so a file created in it has
/// inherited entries whatever ACL the runner's temporary directory carries.
fn set_inheritable_dacl(directory: &Path) {
    let handle = OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC | WRITE_OWNER)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(directory)
        .expect("directory security handle");
    let current = read_security(&handle).expect("directory security");
    let owner = current
        .strip_prefix("O:")
        .and_then(|rest| rest.split("G:").next())
        .expect("owner");
    let prefix = current.split("D:").next().expect("owner and group");
    apply_security(
        &handle,
        &format!("{prefix}D:PAI(A;OICI;FA;;;{owner})(A;OICI;FR;;;BU)"),
    )
    .expect("inheritable DACL");
}

fn current_user_ace_sid(path: &Path) -> String {
    let current = read_security(&security_handle(path)).expect("current security");
    current
        .strip_prefix("O:")
        .and_then(|rest| rest.split("G:").next())
        .expect("owner")
        .to_owned()
}

#[test]
fn publication_replaces_the_name_once_and_keeps_inherited_security() {
    let fixture = fixture();
    set_inheritable_dacl(fixture.path.parent().expect("workspace"));
    fs::write(&fixture.path, b"before").expect("source");
    let target = fixture.target();
    assert!(
        descriptor(&target.capture).contains("D:AI(")
            && descriptor(&target.capture).contains("ID;"),
        "fixture must exercise inherited entries: {}",
        descriptor(&target.capture)
    );
    assert!(
        !descriptor(&target.capture).contains('\0'),
        "the descriptor must stop at its terminator"
    );
    target
        .check_storage(&fixture.directory())
        .expect("same volume preflight");
    let old_id = PrivateFileIdentity {
        volume: target.capture.volume,
        file_id: target.capture.file_id,
    };
    let mut stage = target
        .prepare_stage(fixture.directory(), b"after")
        .expect("stage");
    assert!(target.recheck(&stage).expect("precommit checks"));
    target.commit(&mut stage, || Ok(())).expect("publish");

    assert_eq!(fs::read(&fixture.path).expect("published"), b"after");
    assert_ne!(stage.identity().expect("stage identity"), old_id);
    assert_eq!(
        read_security(&security_handle(&fixture.path)).expect("published security"),
        descriptor(&target.capture)
    );
    assert!(!fixture.stage_path().exists());
    assert_eq!(
        target
            .commit(&mut stage, || Ok(()))
            .expect_err("second attempt")
            .kind(),
        io::ErrorKind::AlreadyExists
    );
}

#[test]
fn publication_keeps_an_explicit_protected_dacl() {
    let fixture = Fixture::with_source(b"before");
    let user = current_user_ace_sid(&fixture.path);
    set_dacl(&fixture.path, &format!("P(A;;FA;;;{user})(A;;FR;;;BU)"));
    let target = fixture.target();
    assert!(descriptor(&target.capture).contains("D:P("));
    let mut stage = target
        .prepare_stage(fixture.directory(), b"after")
        .expect("stage");
    target.commit(&mut stage, || Ok(())).expect("publish");
    assert_eq!(
        read_security(&security_handle(&fixture.path)).expect("published security"),
        descriptor(&target.capture)
    );
}

/// `attrib` sets the opt-out independently of the code under test.
fn set_indexing_opt_out(path: &Path, opted_out: bool) {
    let output = std::process::Command::new("attrib")
        .arg(if opted_out { "+I" } else { "-I" })
        .arg(path)
        .output()
        .expect("attrib");
    assert!(output.status.success(), "indexing fixture applies");
}

fn has_indexing_opt_out(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    fs::metadata(path).expect("metadata").file_attributes()
        & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NOT_CONTENT_INDEXED
        != 0
}

#[test]
fn publication_keeps_the_content_indexing_opt_out_both_ways() {
    for opted_out in [true, false] {
        let fixture = Fixture::with_source(b"before");
        set_indexing_opt_out(&fixture.path, opted_out);
        // The stage must differ from its source for this to prove anything.
        set_indexing_opt_out(&fixture.storage, !opted_out);
        let target = fixture.target();
        let mut stage = target
            .prepare_stage(fixture.directory(), b"after")
            .expect("stage");
        target.commit(&mut stage, || Ok(())).expect("publish");
        assert_eq!(has_indexing_opt_out(&fixture.path), opted_out);
    }
}

#[test]
fn publication_works_when_the_copied_dacl_denies_writing() {
    let fixture = Fixture::with_source(b"before");
    let user = current_user_ace_sid(&fixture.path);
    // Read and execute only: no write data, append, attribute, or delete rights.
    set_dacl(&fixture.path, &format!("P(A;;0x1200a9;;;{user})"));
    let denied = OpenOptions::new()
        .write(true)
        .open(&fixture.path)
        .expect_err("the copied DACL denies writing");
    assert_eq!(denied.kind(), io::ErrorKind::PermissionDenied);
    let target = fixture.target();
    assert!(
        descriptor(target.capture()).ends_with(&format!("D:P(A;;0x1200a9;;;{user})")),
        "capture records the read-only DACL"
    );
    let mut stage = target
        .prepare_stage(fixture.directory(), b"after")
        .expect("stage");
    assert!(
        target
            .recheck(&stage)
            .expect("stage reopens under its DACL")
    );
    target.commit(&mut stage, || Ok(())).expect("publish");
    assert_eq!(fs::read(&fixture.path).expect("published"), b"after");
}

#[test]
fn a_new_stage_is_created_private() {
    let fixture = Fixture::with_source(b"before");
    let directory = fixture.directory();
    let file = directory.create_stage().expect("stage");
    let text = read_security(&file).expect("stage security");
    assert!(
        text.contains("D:P(A;;FA;;;") && text.matches("(A;").count() == 1,
        "new stage must be private: {text}"
    );
}

#[test]
fn capture_reads_bytes_and_security_from_one_handle() {
    let fixture = Fixture::with_source(b"content");
    let capture = fixture
        .workspace
        .capture(&RelativePath::new(Path::new("file")).expect("path"), 1024)
        .expect("capture");
    assert_eq!(capture.bytes, b"content");
    assert_eq!(
        descriptor(&capture),
        read_security(&security_handle(&fixture.path)).expect("security")
    );
}

#[test]
fn a_stage_cannot_publish_to_a_different_retained_source() {
    let fixture = Fixture::with_source(b"same bytes");
    let second_path = fixture.path.with_file_name("other");
    fs::write(&second_path, b"same bytes").expect("second source");
    let first = fixture.target();
    let second = fixture.target_named("other").expect("second target");
    let mut stage = first
        .prepare_stage(fixture.directory(), b"proposed")
        .expect("stage");
    assert!(second.commit(&mut stage, || Ok(())).is_err());
    assert_eq!(fs::read(&fixture.path).expect("first"), b"same bytes");
    assert_eq!(fs::read(&second_path).expect("second"), b"same bytes");
    assert_eq!(fs::read(fixture.stage_path()).expect("stage"), b"proposed");
}

#[test]
fn commit_runs_each_step_in_order_and_cannot_repeat() {
    let fixture = Fixture::with_source(b"before");
    let mut target = fixture.target();
    let events = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&events);
    target.hook = Some(Arc::new(move |point| {
        recorded.lock().expect("event lock").push(point);
        Ok(())
    }));
    let mut stage = target
        .prepare_stage(fixture.directory(), b"after")
        .expect("stage");
    target.commit(&mut stage, || Ok(())).expect("commit");
    assert_eq!(
        *events.lock().expect("events"),
        vec![
            TestPoint::AfterFinalCheck,
            TestPoint::PublishDescriptor,
            TestPoint::Rename,
            TestPoint::PublishedCheck,
            TestPoint::StageSync,
            TestPoint::SourceParentSync,
            TestPoint::TargetParentSync,
        ]
    );
    assert!(target.commit(&mut stage, || Ok(())).is_err());
}

#[test]
fn a_rename_fault_is_one_attempt_and_keeps_the_stage() {
    let fixture = Fixture::with_source(b"before");
    let mut target = fixture.target();
    target.hook = Some(Arc::new(|point| {
        if point == TestPoint::Rename {
            return Err(io::Error::other("injected rename fault"));
        }
        Ok(())
    }));
    let mut stage = target
        .prepare_stage(fixture.directory(), b"after")
        .expect("stage");
    assert!(target.commit(&mut stage, || Ok(())).is_err());
    assert!(
        target.commit(&mut stage, || Ok(())).is_err(),
        "the attempted stage is not retryable"
    );
    assert_eq!(fs::read(&fixture.path).expect("unchanged"), b"before");
    assert!(fixture.stage_path().exists(), "stage evidence remains");
    assert_stage_is_private(&fixture);
}

#[test]
fn a_native_rename_error_leaves_the_target_and_is_not_retried() {
    let fixture = Fixture::with_source(b"before");
    let mut target = fixture.target();
    let held_path = fixture.path.clone();
    let held = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&held);
    target.hook = Some(Arc::new(move |point| {
        if point == TestPoint::AfterFinalCheck {
            // A reader without delete sharing makes the kernel refuse the rename.
            let file = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(&held_path)?;
            *slot.lock().expect("slot") = Some(file);
        }
        Ok(())
    }));
    let mut stage = target
        .prepare_stage(fixture.directory(), b"after")
        .expect("stage");
    assert!(target.commit(&mut stage, || Ok(())).is_err());
    assert_eq!(
        target
            .commit(&mut stage, || Ok(()))
            .expect_err("attempt consumed")
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    drop(held.lock().expect("slot").take());
    assert_eq!(fs::read(&fixture.path).expect("unchanged"), b"before");
    assert_eq!(fs::read(fixture.stage_path()).expect("stage"), b"after");
    assert_stage_is_private(&fixture);
}

#[test]
fn a_last_moment_denial_preserves_target_and_stage() {
    let fixture = Fixture::with_source(b"before");
    let target = fixture.target();
    let mut stage = target
        .prepare_stage(fixture.directory(), b"after")
        .expect("stage");
    let error = target
        .commit(&mut stage, || {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "authority expired",
            ))
        })
        .expect_err("expired authority refuses before rename");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(fs::read(&fixture.path).expect("unchanged"), b"before");
    assert!(fixture.stage_path().exists(), "stage evidence remains");
    assert_stage_is_private(&fixture);
    assert!(
        target.commit(&mut stage, || Ok(())).is_err(),
        "a refused attempt cannot be retried"
    );
}

/// The retained stage has exactly the owner-only DACL of the private store.
fn assert_stage_is_private(fixture: &Fixture) {
    let stage = File::open(fixture.stage_path()).expect("open stage");
    assert!(
        is_private_file(&stage).expect("stage security"),
        "the stage must be private: {}",
        read_security(&stage).expect("stage security")
    );
}

#[test]
fn the_stage_stays_private_until_commit_while_the_source_grants_others() {
    let fixture = Fixture::with_source(b"before");
    let user = current_user_ace_sid(&fixture.path);
    set_dacl(&fixture.path, &format!("P(A;;FA;;;{user})(A;;FR;;;BU)"));
    let target = fixture.target();
    let source = descriptor(target.capture()).to_owned();
    assert!(
        source.contains(";;;BU)"),
        "the source grants Users: {source}"
    );
    let mut stage = target
        .prepare_stage(fixture.directory(), b"after")
        .expect("stage");
    assert_stage_is_private(&fixture);
    assert!(target.recheck(&stage).expect("unchanged target"));
    assert_stage_is_private(&fixture);
    target.commit(&mut stage, || Ok(())).expect("publish");
    assert_eq!(fs::read(&fixture.path).expect("published"), b"after");
    assert_eq!(
        read_security(&security_handle(&fixture.path)).expect("published security"),
        source,
        "only the published file carries the source DACL"
    );
}

#[test]
fn a_failed_restore_is_reported_and_leaves_the_source_security() {
    let fixture = Fixture::with_source(b"before");
    let user = current_user_ace_sid(&fixture.path);
    set_dacl(&fixture.path, &format!("P(A;;FA;;;{user})(A;;FR;;;BU)"));
    let mut target = fixture.target();
    target.hook = Some(Arc::new(|point| match point {
        TestPoint::Rename => Err(io::Error::other("injected rename fault")),
        TestPoint::RestorePrivacy => Err(io::Error::other("injected restore fault")),
        _ => Ok(()),
    }));
    let source = descriptor(target.capture()).to_owned();
    let mut stage = target
        .prepare_stage(fixture.directory(), b"after")
        .expect("stage");
    let error = target
        .commit(&mut stage, || Ok(()))
        .expect_err("rename fault");
    assert!(
        error
            .to_string()
            .contains("the stage may keep the source security"),
        "a failed restore must be reported: {error}"
    );
    assert_eq!(fs::read(&fixture.path).expect("unchanged"), b"before");
    let retained = File::open(fixture.stage_path()).expect("open stage");
    assert!(!is_private_file(&retained).expect("stage security"));
    assert_eq!(read_security(&retained).expect("stage security"), source);
}

/// The opt-out changes while the stage is still private, so a crash between
/// the two metadata writes cannot leave the source DACL with the old opt-out.
#[test]
fn the_indexing_opt_out_changes_only_while_the_stage_is_private() {
    let fixture = Fixture::with_source(b"before");
    let user = current_user_ace_sid(&fixture.path);
    set_dacl(&fixture.path, &format!("P(A;;FA;;;{user})(A;;FR;;;BU)"));
    set_indexing_opt_out(&fixture.path, true);
    set_indexing_opt_out(&fixture.storage, false);
    let stage_path = fixture.stage_path();
    let observed = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&observed);
    let mut target = fixture.target();
    target.hook = Some(Arc::new(move |point| {
        if point != TestPoint::PublishDescriptor {
            return Ok(());
        }
        let stage = OpenOptions::new()
            .access_mode(READ_CONTROL)
            .open(&stage_path)?;
        *seen.lock().expect("observation") =
            Some((is_private_file(&stage)?, has_indexing_opt_out(&stage_path)));
        Err(io::Error::other(
            "injected crash between the metadata writes",
        ))
    }));
    let mut stage = target
        .prepare_stage(fixture.directory(), b"after")
        .expect("stage");
    assert!(
        !has_indexing_opt_out(&fixture.stage_path()),
        "the stage must start with the other opt-out"
    );
    target
        .commit(&mut stage, || Ok(()))
        .expect_err("injected fault");
    assert_eq!(
        *observed.lock().expect("observation"),
        Some((true, true)),
        "between the writes the stage is private and has the source opt-out"
    );
    assert_eq!(fs::read(&fixture.path).expect("unchanged"), b"before");
    assert_stage_is_private(&fixture);
    assert!(
        !has_indexing_opt_out(&fixture.stage_path()),
        "restoring privacy also restores the stage's opt-out"
    );
}

#[test]
fn a_conflict_retains_a_private_stage() {
    let fixture = Fixture::with_source(b"same");
    let user = current_user_ace_sid(&fixture.path);
    set_dacl(&fixture.path, &format!("P(A;;FA;;;{user})(A;;FR;;;BU)"));
    let target = fixture.target();
    let stage = target
        .prepare_stage(fixture.directory(), b"new")
        .expect("stage");
    fs::remove_file(&fixture.path).expect("remove source");
    assert!(!target.recheck(&stage).expect("conflict"));
    drop(stage);
    assert_stage_is_private(&fixture);
}

#[test]
fn failures_after_the_rename_stay_uncertain_and_keep_the_visible_state() {
    for fault in [
        TestPoint::PublishedCheck,
        TestPoint::StageSync,
        TestPoint::SourceParentSync,
        TestPoint::TargetParentSync,
    ] {
        let fixture = Fixture::with_source(b"before");
        let mut target = fixture.target();
        target.hook = Some(Arc::new(move |point| {
            if point == fault {
                return Err(io::Error::other("injected post-rename failure"));
            }
            Ok(())
        }));
        let mut stage = target
            .prepare_stage(fixture.directory(), b"after")
            .expect("stage");
        assert!(
            target.commit(&mut stage, || Ok(())).is_err(),
            "{fault:?} is uncertain"
        );
        assert_eq!(fs::read(&fixture.path).expect("published"), b"after");
        assert!(!fixture.stage_path().exists(), "stage moved to the target");
        assert!(
            target.commit(&mut stage, || Ok(())).is_err(),
            "{fault:?} is not retryable"
        );
    }
}

#[test]
fn a_swap_after_the_final_check_is_overwritten_and_not_claimed_as_cas() {
    let fixture = Fixture::with_source(b"captured");
    let mut target = fixture.target();
    let changed_path = fixture.path.clone();
    target.hook = Some(Arc::new(move |point| {
        if point == TestPoint::AfterFinalCheck {
            fs::remove_file(&changed_path)?;
            fs::write(&changed_path, b"independent edit")?;
        }
        Ok(())
    }));
    let mut stage = target
        .prepare_stage(fixture.directory(), b"proposal")
        .expect("stage");
    target
        .commit(&mut stage, || Ok(()))
        .expect("rename can overwrite a later swap");
    assert_eq!(fs::read(&fixture.path).expect("published"), b"proposal");
    assert_eq!(target.capture().bytes, b"captured");
}

#[test]
fn recheck_rejects_equal_bytes_in_a_different_file() {
    let fixture = Fixture::with_source(b"same");
    let target = fixture.target();
    let stage = target
        .prepare_stage(fixture.directory(), b"new")
        .expect("stage");
    fs::remove_file(&fixture.path).expect("remove source");
    fs::write(&fixture.path, b"same").expect("equal bytes, new file");
    assert!(!target.recheck(&stage).expect("identity conflict"));
}

#[test]
fn recheck_rejects_a_removed_target() {
    let fixture = Fixture::with_source(b"same");
    let target = fixture.target();
    let stage = target
        .prepare_stage(fixture.directory(), b"new")
        .expect("stage");
    fs::remove_file(&fixture.path).expect("remove source");
    assert!(!target.recheck(&stage).expect("missing target"));
}

#[test]
fn recheck_rejects_changed_security() {
    let fixture = Fixture::with_source(b"same");
    let target = fixture.target();
    let stage = target
        .prepare_stage(fixture.directory(), b"new")
        .expect("stage");
    let user = current_user_ace_sid(&fixture.path);
    set_dacl(&fixture.path, &format!("P(A;;FA;;;{user})"));
    assert!(!target.recheck(&stage).expect("security conflict"));
}

#[test]
fn recheck_rejects_changed_security_with_restored_times() {
    let fixture = Fixture::with_source(b"same");
    let target = fixture.target();
    let stage = target
        .prepare_stage(fixture.directory(), b"new")
        .expect("stage");
    let times = query_basic(&target.source).expect("times");
    let user = current_user_ace_sid(&fixture.path);
    set_dacl(&fixture.path, &format!("P(A;;FA;;;{user})"));
    restore_times(&fixture.path, &times);
    assert_source_snapshot_is_unchanged(&target);
    assert!(!target.recheck(&stage).expect("security conflict"));
}

#[test]
fn recheck_rejects_a_new_stream_with_restored_times() {
    let fixture = Fixture::with_source(b"same");
    let target = fixture.target();
    let stage = target
        .prepare_stage(fixture.directory(), b"new")
        .expect("stage");
    let times = query_basic(&target.source).expect("times");
    let stream = format!("{}:extra", fixture.path.display());
    fs::write(stream, b"hidden").expect("alternate stream");
    restore_times(&fixture.path, &times);
    assert_source_snapshot_is_unchanged(&target);
    assert!(!target.recheck(&stage).expect("stream conflict"));
}

#[test]
fn recheck_rejects_a_different_file_while_the_source_is_kept() {
    let fixture = Fixture::with_source(b"same");
    let target = fixture.target();
    let stage = target
        .prepare_stage(fixture.directory(), b"new")
        .expect("stage");
    let times = query_basic(&target.source).expect("times");
    let aside = fixture.path.with_file_name("aside");
    fs::rename(&fixture.path, &aside).expect("move source aside");
    restore_times(&aside, &times);
    fs::write(&fixture.path, b"same").expect("equal bytes, new file");
    assert_source_snapshot_is_unchanged(&target);
    assert!(!target.recheck(&stage).expect("identity conflict"));
}

#[test]
fn recheck_rejects_a_changed_name_case() {
    let fixture = Fixture::with_source(b"same");
    let target = fixture.target();
    let stage = target
        .prepare_stage(fixture.directory(), b"new")
        .expect("stage");
    let times = query_basic(&target.source).expect("times");
    let renamed = fixture.path.with_file_name("FILE");
    fs::rename(&fixture.path, &renamed).expect("change case");
    restore_times(&renamed, &times);
    assert_source_snapshot_is_unchanged(&target);
    assert!(!target.recheck(&stage).expect("name conflict"));
}

/// Put a file's times back, as anyone with write-attributes access can.
#[allow(unsafe_code)]
fn restore_times(path: &Path, times: &FILE_BASIC_INFO) {
    let file = OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES | SYNCHRONIZE)
        .open(path)
        .expect("attributes handle");
    // SAFETY: `times` is a live `FILE_BASIC_INFO`, the class this call expects,
    // and the handle stays open for this synchronous call.
    let set = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle() as HANDLE,
            FileBasicInfo,
            std::ptr::from_ref(times).cast(),
            u32::try_from(size_of::<FILE_BASIC_INFO>()).expect("information size"),
        )
    };
    assert_ne!(set, 0, "times restored");
}

/// Show that the retained snapshot cannot be what refuses the change under test.
fn assert_source_snapshot_is_unchanged(target: &ReplacementTarget<'_>) {
    assert!(
        snapshot_matches(&target.source, target.source_snapshot).expect("snapshot"),
        "the fixture must leave the retained snapshot unchanged"
    );
}

#[test]
fn a_hard_linked_stage_cannot_be_opened() {
    let fixture = fixture();
    let directory = fixture.directory();
    drop(directory.create_stage().expect("stage"));
    assert!(directory.open_stage().is_ok(), "a plain stage opens");
    fs::hard_link(fixture.stage_path(), fixture.storage.join("alias")).expect("hard link");
    let error = directory.open_stage().expect_err("hard link");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn a_stage_with_an_alternate_stream_cannot_be_opened() {
    let fixture = fixture();
    let directory = fixture.directory();
    drop(directory.create_stage().expect("stage"));
    assert!(directory.open_stage().is_ok(), "a plain stage opens");
    let stream = format!("{}:extra", fixture.stage_path().display());
    fs::write(stream, b"hidden").expect("alternate stream");
    let error = directory.open_stage().expect_err("alternate stream");
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
}

#[test]
fn a_tampered_stage_is_refused() {
    let fixture = Fixture::with_source(b"same");
    let target = fixture.target();
    let stage = target
        .prepare_stage(fixture.directory(), b"new")
        .expect("stage");
    let user = current_user_ace_sid(&fixture.stage_path());
    set_dacl(
        &fixture.stage_path(),
        &format!("P(A;;FA;;;{user})(A;;FR;;;BU)"),
    );
    assert!(target.recheck(&stage).is_err());
}

#[test]
fn the_retained_source_blocks_new_writers() {
    let fixture = Fixture::with_source(b"same");
    let _target = fixture.target();
    let error = OpenOptions::new()
        .write(true)
        .share_mode(SHARE_ALL)
        .open(&fixture.path)
        .expect_err("writer is blocked");
    assert_eq!(error.raw_os_error(), Some(32), "sharing violation");
}

#[test]
fn an_open_writer_refuses_capture() {
    let fixture = Fixture::with_source(b"same");
    let _writer = OpenOptions::new()
        .write(true)
        .share_mode(SHARE_ALL)
        .open(&fixture.path)
        .expect("writer");
    assert!(fixture.target_named("file").is_err());
}

#[test]
fn a_reader_without_delete_sharing_refuses_replacement_but_not_capture() {
    let fixture = Fixture::with_source(b"same");
    let _reader = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&fixture.path)
        .expect("reader");
    fixture
        .workspace
        .capture(&RelativePath::new(Path::new("file")).expect("path"), 1024)
        .expect("checkpoint capture does not need delete access");
    assert!(fixture.target_named("file").is_err());
}

fn assert_unsupported_source(fixture: &Fixture) {
    assert_eq!(
        fixture
            .target_named("file")
            .err()
            .expect("unsupported source")
            .kind(),
        io::ErrorKind::Unsupported
    );
}

#[test]
fn alternate_streams_are_refused() {
    let fixture = Fixture::with_source(b"same");
    let stream = format!("{}:extra", fixture.path.display());
    fs::write(stream, b"hidden").expect("alternate stream");
    assert_unsupported_source(&fixture);
}

/// Attach one extended attribute through the native call that writes them.
#[allow(unsafe_code)]
fn set_extended_attribute(path: &Path) {
    use windows_sys::Wdk::Storage::FileSystem::{FILE_FULL_EA_INFORMATION, NtSetEaFile};
    use windows_sys::Win32::Storage::FileSystem::FILE_WRITE_EA;

    let file = OpenOptions::new()
        .access_mode(FILE_WRITE_EA | SYNCHRONIZE)
        .open(path)
        .expect("EA handle");
    let name = b"OMGTEST";
    let value = b"x";
    let start = offset_of!(FILE_FULL_EA_INFORMATION, EaName);
    let mut record = vec![0u8; start + name.len() + 1 + value.len()];
    record[5] = u8::try_from(name.len()).expect("name length");
    record[6..8].copy_from_slice(&u16::try_from(value.len()).expect("value").to_le_bytes());
    record[start..start + name.len()].copy_from_slice(name);
    record[start + name.len() + 1..].copy_from_slice(value);
    // The native call expects 4-byte alignment, which a `Vec<u32>` guarantees.
    let mut storage = vec![0u32; record.len().div_ceil(size_of::<u32>())];
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: `storage` holds `record.len()` writable bytes, and the file handle
    // and status block stay live for this synchronous call.
    let status = unsafe {
        std::ptr::copy_nonoverlapping(
            record.as_ptr(),
            storage.as_mut_ptr().cast::<u8>(),
            record.len(),
        );
        NtSetEaFile(
            file.as_raw_handle() as HANDLE,
            &mut status_block,
            storage.as_ptr().cast(),
            u32::try_from(record.len()).expect("record length"),
        )
    };
    assert_eq!(status, 0, "EA fixture applies");
}

#[test]
fn extended_attributes_are_refused() {
    let fixture = Fixture::with_source(b"same");
    set_extended_attribute(&fixture.path);
    assert_unsupported_source(&fixture);
}

#[test]
fn read_only_files_are_refused() {
    let fixture = Fixture::with_source(b"same");
    let mut permissions = fs::metadata(&fixture.path).expect("metadata").permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&fixture.path, permissions.clone()).expect("read-only");
    assert_unsupported_source(&fixture);
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "the fixture must clear the attribute so cleanup can remove the file"
    )]
    permissions.set_readonly(false);
    fs::set_permissions(&fixture.path, permissions).expect("writable");
}

#[test]
fn hidden_files_are_refused() {
    let fixture = fixture();
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .attributes(FILE_ATTRIBUTE_HIDDEN)
        .open(&fixture.path)
        .expect("hidden file");
    assert_unsupported_source(&fixture);
}

#[test]
fn mandatory_labels_are_refused() {
    let fixture = Fixture::with_source(b"same");
    let status = std::process::Command::new("icacls")
        .arg(&fixture.path)
        .args(["/setintegritylevel", "L"])
        .output()
        .expect("icacls");
    assert!(status.status.success(), "label fixture applies");
    assert_unsupported_source(&fixture);
}

#[test]
fn hard_links_and_junctions_are_refused() {
    let fixture = Fixture::with_source(b"same");
    fs::hard_link(&fixture.path, fixture.path.with_file_name("alias")).expect("hard link");
    assert_eq!(
        fixture
            .target_named("file")
            .err()
            .expect("hard link")
            .kind(),
        io::ErrorKind::InvalidData
    );
    let outside = tempfile::tempdir().expect("outside");
    create_junction_bounded(&fixture.path.with_file_name("link"), outside.path())
        .expect("junction");
    assert!(fixture.target_named("link").is_err());
}

#[test]
fn a_different_case_is_refused() {
    let fixture = Fixture::with_source(b"same");
    assert_eq!(
        fixture
            .target_named("FILE")
            .err()
            .expect("case alias")
            .kind(),
        io::ErrorKind::Unsupported
    );
    assert!(fixture.path.exists());
}

#[test]
fn oversized_sources_and_proposals_are_refused() {
    let fixture = Fixture::with_source(&[b'x'; 2048]);
    assert!(fixture.target_named("file").is_err());
    fs::write(&fixture.path, b"small").expect("small source");
    let target = fixture.target();
    assert_eq!(
        target
            .prepare_stage(fixture.directory(), &vec![0u8; MAX_FILE_BYTES + 1])
            .err()
            .expect("oversized proposal")
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(!fixture.stage_path().exists());
}

#[test]
fn storage_and_target_must_share_a_volume() {
    assert!(validate_same_volume(7, 7, 8).is_err());
    assert!(validate_same_volume(7, 6, 7).is_err());
    assert!(validate_same_volume(7, 7, 7).is_ok());
}

#[test]
fn a_junction_stage_name_is_refused() {
    let fixture = fixture();
    let outside = tempfile::tempdir().expect("outside");
    create_junction_bounded(&fixture.stage_path(), outside.path()).expect("junction");
    assert!(fixture.directory().open_stage().is_err());
}

#[test]
fn name_records_return_the_last_component_and_reject_truncation() {
    let units: Vec<u16> = "\\a\\file".encode_utf16().collect();
    let mut bytes = u32::try_from(units.len() * 2)
        .expect("length")
        .to_le_bytes()
        .to_vec();
    for unit in &units {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let expected: Vec<u16> = "file".encode_utf16().collect();
    assert_eq!(final_component(&bytes).expect("name"), expected);
    bytes.truncate(8);
    assert!(final_component(&bytes).is_err());
    assert!(final_component(&[0u8; 2]).is_err());
}
