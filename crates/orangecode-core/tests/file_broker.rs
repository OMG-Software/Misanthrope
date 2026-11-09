use std::fs;
use std::path::{Path, PathBuf};

use orangecode_core::control::ControlStore;
use orangecode_core::files::{FileBroker, FileError, FileTarget};
use orangecode_core::policy::{DecisionReason, PermissionSet, SessionBinding, WorkspaceId};
use orangecode_core::session::NativePath;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use uuid::Uuid;

const MAX_FILE_BYTES: usize = 1024 * 1024;
const FILE_AUDIT_NAME: &str = "file-operations-v1.jsonl";

/// Unpaired UTF-16 units must survive native I/O, audit replay and listing metadata.
#[cfg(windows)]
#[test]
fn windows_unpaired_surrogate_names_survive_creation_read_and_listing() {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    let mut fixture = Fixture::new();
    let binding = fixture.binding();
    let relative = PathBuf::from(OsString::from_wide(&[102, 0xd800, 46, 116, 120, 116]));
    let target = target(fixture.session_id, fixture.workspace_id, relative.clone());
    let mut broker = FileBroker::new(&mut fixture.control);
    let created = broker
        .create_file(&binding, &target, b"native\0bytes")
        .expect("native create");
    assert_eq!(path_from_native(&created.target), relative);
    assert_eq!(
        broker
            .read(&binding, &target, 64)
            .expect("native read")
            .bytes,
        b"native\0bytes"
    );
    let listing_target = FileTarget {
        session_id: fixture.session_id,
        workspace_id: fixture.workspace_id,
        path: PathBuf::new(),
    };
    let listing = broker
        .list(&binding, &listing_target, 16)
        .expect("native listing");
    assert!(
        listing
            .entries
            .iter()
            .any(|entry| path_from_native(&entry.name) == relative)
    );
}

struct Fixture {
    temporary: TempDir,
    data: PathBuf,
    root: PathBuf,
    workspace: PathBuf,
    workspace_id: WorkspaceId,
    other_workspace_id: WorkspaceId,
    session_id: Uuid,
    control: ControlStore,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("private temporary directory");
        let data = temporary.path().join("application-data");
        let root = temporary.path().join("development-root");
        let workspace = root.join("workspace");
        let other_workspace = root.join("other-workspace");
        fs::create_dir(&root).expect("create development root");
        fs::create_dir(&workspace).expect("create workspace");
        fs::create_dir(&other_workspace).expect("create second workspace");

        let mut control = ControlStore::open(&data).expect("open control store");
        control
            .set_development_root(&root, control.revision())
            .expect("select development root");
        let workspace_registration = control
            .register_workspace(&workspace, control.revision())
            .expect("register workspace");
        let other_registration = control
            .register_workspace(&other_workspace, control.revision())
            .expect("register second workspace");

        Self {
            temporary,
            data,
            root,
            workspace,
            workspace_id: workspace_registration.id(),
            other_workspace_id: other_registration.id(),
            session_id: Uuid::new_v4(),
            control,
        }
    }

    fn binding(&self) -> SessionBinding {
        self.control
            .bind_session(self.session_id, self.workspace_id)
            .expect("bind workspace session")
    }

    fn file_audit_path(&self) -> PathBuf {
        self.data.join(FILE_AUDIT_NAME)
    }
}

fn target(session_id: Uuid, workspace_id: WorkspaceId, path: impl Into<PathBuf>) -> FileTarget {
    FileTarget {
        session_id,
        workspace_id,
        path: path.into(),
    }
}

fn path_from_native(path: &NativePath) -> PathBuf {
    path.to_path_buf()
        .expect("native path belongs to this host")
}

fn file_operation_events<'a>(
    entries: &'a [serde_json::Value],
    action: &str,
) -> Vec<&'a serde_json::Value> {
    entries
        .iter()
        .filter(|entry| entry["action"]["type"] == action)
        .collect()
}

/// A complete operation set returns exact bytes and durable metadata only.
#[test]
fn broker_reads_binary_data_lists_and_creates_with_content_free_audit() {
    // Given private application data and two independent workspace subtrees.
    let mut fixture = Fixture::new();
    let binary = b"\x00\xff\x80broker-binary-data";
    let binary_path = fixture.workspace.join("binary.dat");
    fs::write(&binary_path, binary).expect("write binary fixture");
    let secret = b"FILE-CONTENT-MUST-NOT-ENTER-AUDIT-7e1932";
    let binding = fixture.binding();

    // When the broker reads, creates, and lists through the live binding.
    let (read, created_file, created_directory, listing, audit) = {
        let mut broker = FileBroker::new(&mut fixture.control);
        let read = broker
            .read(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, "binary.dat"),
                128,
            )
            .expect("read binary file");
        let created_file = broker
            .create_file(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, "created.txt"),
                secret,
            )
            .expect("create exclusive file");
        let created_directory = broker
            .create_directory(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, "created-dir"),
            )
            .expect("create one directory");
        let listing = broker
            .list(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, ""),
                10,
            )
            .expect("list workspace root");
        let audit = broker.audit(100).expect("read durable operation audit");
        (read, created_file, created_directory, listing, audit)
    };

    // Then the broker returns exact file data and its matching digest.
    assert_eq!(read.bytes, binary);
    let expected_digest = Sha256::digest(binary)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(read.sha256, expected_digest);
    assert_eq!(created_file.bytes_written, secret.len());
    assert_eq!(created_directory.bytes_written, 0);
    assert_eq!(
        fs::read(fixture.workspace.join("created.txt")).expect("created bytes"),
        secret
    );
    assert!(fixture.workspace.join("created-dir").is_dir());

    let listed: Vec<_> = listing
        .entries
        .iter()
        .map(|entry| (path_from_native(&entry.name), entry.kind))
        .collect();
    assert!(
        listed
            .iter()
            .any(|(name, kind)| { name == Path::new("binary.dat") && *kind == "file" })
    );
    assert!(
        listed
            .iter()
            .any(|(name, kind)| { name == Path::new("created-dir") && *kind == "directory" })
    );
    assert!(!listing.truncated);

    // Durable audit contains the four completed operations and no file bytes.
    assert_eq!(audit.pending, None);
    assert_eq!(
        file_operation_events(&audit.entries, "operation_started").len(),
        4
    );
    let finished = file_operation_events(&audit.entries, "operation_finished");
    assert_eq!(finished.len(), 4);
    assert!(
        finished
            .iter()
            .all(|entry| { entry["action"]["payload"]["outcome"] == "succeeded" })
    );
    let encoded_audit = serde_json::to_string(&audit.entries).expect("encode audit view");
    assert!(!encoded_audit.contains(std::str::from_utf8(secret).expect("test marker")));
    assert!(!encoded_audit.contains("broker-binary-data"));
}

/// A settings revision change invalidates old bindings; new flags control I/O.
#[test]
fn read_only_and_write_only_preferences_require_fresh_bindings() {
    // Given an existing file and read-only preferences.
    let mut fixture = Fixture::new();
    fs::write(fixture.workspace.join("existing.txt"), b"original").expect("write existing file");
    fixture
        .control
        .set_permissions(
            fixture.workspace_id,
            PermissionSet {
                read: true,
                write: false,
                ..PermissionSet::default()
            },
            fixture.control.revision(),
        )
        .expect("set read-only flags");
    let read_only_binding = fixture.binding();

    // When read and write operations run under that binding.
    {
        let mut broker = FileBroker::new(&mut fixture.control);
        assert_eq!(
            broker
                .read(
                    &read_only_binding,
                    &target(fixture.session_id, fixture.workspace_id, "existing.txt"),
                    100
                )
                .expect("read is allowed")
                .bytes,
            b"original"
        );
        assert!(matches!(
            broker.create_file(
                &read_only_binding,
                &target(fixture.session_id, fixture.workspace_id, "denied.txt"),
                b"no"
            ),
            Err(FileError::Denied(DecisionReason::PermissionDisabled))
        ));
    }

    // When settings change to write-only, the earlier binding is stale.
    fixture
        .control
        .set_permissions(
            fixture.workspace_id,
            PermissionSet {
                read: false,
                write: true,
                ..PermissionSet::default()
            },
            fixture.control.revision(),
        )
        .expect("set write-only flags");
    let stale = {
        let mut broker = FileBroker::new(&mut fixture.control);
        broker
            .read(
                &read_only_binding,
                &target(fixture.session_id, fixture.workspace_id, "existing.txt"),
                100,
            )
            .expect_err("changed settings stale the binding")
    };
    assert!(matches!(
        stale,
        FileError::Denied(DecisionReason::StaleRevision)
    ));

    // Then a new binding permits creation but denies reads.
    let write_only_binding = fixture.binding();
    {
        let mut broker = FileBroker::new(&mut fixture.control);
        assert!(matches!(
            broker.read(
                &write_only_binding,
                &target(fixture.session_id, fixture.workspace_id, "existing.txt"),
                100
            ),
            Err(FileError::Denied(DecisionReason::PermissionDisabled))
        ));
        broker
            .create_file(
                &write_only_binding,
                &target(fixture.session_id, fixture.workspace_id, "new.txt"),
                b"created",
            )
            .expect("write is allowed");
    }
    assert_eq!(
        fs::read(fixture.workspace.join("existing.txt")).unwrap(),
        b"original"
    );
    assert_eq!(
        fs::read(fixture.workspace.join("new.txt")).unwrap(),
        b"created"
    );
    assert!(!fixture.workspace.join("denied.txt").exists());
}

/// Session, workspace, store, and revision mismatches fail before file I/O.
#[test]
fn scope_mismatches_are_denied_without_creating_operation_audit_or_files() {
    // Given a valid binding and a second independently registered workspace.
    let mut fixture = Fixture::new();
    let binding = fixture.binding();
    let missing_path = PathBuf::from("must-not-exist.txt");

    // When the request uses another session or workspace.
    {
        let mut broker = FileBroker::new(&mut fixture.control);
        let mut other_session = target(fixture.session_id, fixture.workspace_id, &missing_path);
        other_session.session_id = Uuid::new_v4();
        assert!(matches!(
            broker.read(&binding, &other_session, 10),
            Err(FileError::Denied(DecisionReason::SessionMismatch))
        ));
        assert!(matches!(
            broker.read(
                &binding,
                &target(
                    fixture.session_id,
                    fixture.other_workspace_id,
                    &missing_path
                ),
                10
            ),
            Err(FileError::Denied(DecisionReason::WorkspaceMismatch))
        ));
    }

    // A binding from another store is rejected even when the request names this workspace.
    let mut foreign = ControlStore::open(&fixture.temporary.path().join("foreign-data"))
        .expect("open independent control store");
    foreign
        .set_development_root(&fixture.root, foreign.revision())
        .expect("select shared development root");
    let foreign_workspace = foreign
        .register_workspace(&fixture.workspace, foreign.revision())
        .expect("register same directory in the foreign store");
    let foreign_binding = foreign
        .bind_session(fixture.session_id, foreign_workspace.id())
        .expect("bind foreign store session");
    {
        let mut broker = FileBroker::new(&mut fixture.control);
        assert!(matches!(
            broker.read(
                &foreign_binding,
                &target(fixture.session_id, fixture.workspace_id, &missing_path),
                10
            ),
            Err(FileError::Denied(DecisionReason::StoreMismatch))
        ));
    }
    drop(foreign);

    // A settings revision change makes the original binding stale.
    fixture
        .control
        .set_permissions(
            fixture.workspace_id,
            PermissionSet {
                task_network: true,
                ..PermissionSet::default()
            },
            fixture.control.revision(),
        )
        .expect("change settings revision");
    {
        let mut broker = FileBroker::new(&mut fixture.control);
        assert!(matches!(
            broker.read(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, &missing_path),
                10
            ),
            Err(FileError::Denied(DecisionReason::StaleRevision))
        ));
    }

    // Then every denied request has no file-system side effect or file operation record.
    assert!(!fixture.workspace.join(&missing_path).exists());
    assert!(!fixture.file_audit_path().exists());
}

/// Invalid paths and links cannot disclose bytes from outside the workspace.
#[test]
fn traversal_absolute_and_hard_link_paths_cannot_read_outside_data() {
    // Given an outside sentinel and a hard link to it inside the workspace.
    let mut fixture = Fixture::new();
    let outside = fixture.temporary.path().join("outside-secret.txt");
    let sentinel = b"outside-sentinel-must-remain-unchanged";
    fs::write(&outside, sentinel).expect("write outside sentinel");
    fs::hard_link(&outside, fixture.workspace.join("outside-hardlink"))
        .expect("create outside hard link");
    let binding = fixture.binding();

    // When paths attempt traversal, use an absolute name, or name the hard link.
    {
        let mut broker = FileBroker::new(&mut fixture.control);
        for path in [PathBuf::from("../outside-secret.txt"), outside.clone()] {
            assert!(matches!(
                broker.read(&binding, &target(fixture.session_id, fixture.workspace_id, path), 100),
                Err(FileError::Native(error)) if error.kind() == std::io::ErrorKind::InvalidInput
            ));
        }
        assert!(matches!(
            broker.read(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, "outside-hardlink"),
                100
            ),
            Err(FileError::Native(_))
        ));
    }

    // Then the outside name retains its original bytes and no content was returned.
    assert_eq!(
        fs::read(outside).expect("outside sentinel remains readable"),
        sentinel
    );
}

/// A symlink to external data is blocked and appears as a non-readable entry.
#[cfg(unix)]
#[test]
fn symbolic_links_are_blocked_without_following_their_targets() {
    // Given an outside file and a final-component link inside the workspace.
    let mut fixture = Fixture::new();
    let outside = fixture.temporary.path().join("outside.txt");
    fs::write(&outside, b"outside bytes").expect("write outside file");
    std::os::unix::fs::symlink(&outside, fixture.workspace.join("outside-link"))
        .expect("create final link");
    let binding = fixture.binding();

    // When the broker reads and lists the link.
    let (read_error, listing) = {
        let mut broker = FileBroker::new(&mut fixture.control);
        let read_error = broker
            .read(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, "outside-link"),
                100,
            )
            .expect_err("symbolic link cannot supply file bytes");
        let listing = broker
            .list(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, ""),
                10,
            )
            .expect("list names without following links");
        (read_error, listing)
    };

    // Then reading fails and listing reports the link as blocked.
    assert!(matches!(read_error, FileError::Native(_)));
    assert!(listing.entries.iter().any(|entry| {
        path_from_native(&entry.name) == Path::new("outside-link") && entry.kind == "blocked"
    }));
    assert_eq!(
        fs::read(outside).expect("outside target remains readable"),
        b"outside bytes"
    );
}

/// A stale workspace registration fails after the saved directory is replaced.
#[test]
fn replaced_workspace_is_denied_before_replacement_data_is_read() {
    // Given a binding to a workspace with one private sentinel.
    let mut fixture = Fixture::new();
    fs::write(
        fixture.workspace.join("sentinel.txt"),
        b"original workspace",
    )
    .expect("write registered workspace sentinel");
    let binding = fixture.binding();

    // When the original directory moves and a new directory occupies its old path.
    let moved = fixture.root.join("moved-workspace");
    fs::rename(&fixture.workspace, moved).expect("move original workspace");
    fs::create_dir(&fixture.workspace).expect("replace saved workspace path");
    fs::write(fixture.workspace.join("sentinel.txt"), b"replacement data")
        .expect("write replacement sentinel");

    // Then the stale identity is denied and replacement bytes are not returned.
    let error = {
        let mut broker = FileBroker::new(&mut fixture.control);
        broker
            .read(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, "sentinel.txt"),
                100,
            )
            .expect_err("replaced directory must not inherit the binding")
    };
    assert!(matches!(
        error,
        FileError::Denied(DecisionReason::WorkspaceUnavailable)
            | FileError::Control(orangecode_core::control::ControlError::DirectoryChanged)
    ));
    assert_eq!(
        fs::read(fixture.workspace.join("sentinel.txt")).expect("read replacement fixture"),
        b"replacement data"
    );
}

/// Exclusive creation preserves old bytes and empty paths are listing-only.
#[test]
fn collisions_preserve_bytes_and_only_root_listing_accepts_an_empty_path() {
    // Given an existing file and a valid read/write binding.
    let mut fixture = Fixture::new();
    let original = b"original bytes stay exact";
    fs::write(fixture.workspace.join("collision.txt"), original).expect("write old file");
    let binding = fixture.binding();

    // When create collides, and the empty path is used for each public operation.
    let (collision, root_listing, empty_read, empty_create) = {
        let mut broker = FileBroker::new(&mut fixture.control);
        let collision = broker
            .create_file(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, "collision.txt"),
                b"replacement",
            )
            .expect_err("exclusive create must refuse collisions");
        let root_listing = broker
            .list(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, ""),
                10,
            )
            .expect("empty path lists workspace root");
        let empty_read = broker
            .read(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, ""),
                10,
            )
            .expect_err("empty path cannot name a file");
        let empty_create = broker
            .create_file(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, ""),
                b"invalid",
            )
            .expect_err("empty path cannot create a file");
        (collision, root_listing, empty_read, empty_create)
    };

    // Then the existing file is unchanged and invalid empty targets are rejected.
    assert!(matches!(collision, FileError::Conflict));
    assert_eq!(
        fs::read(fixture.workspace.join("collision.txt")).expect("read old bytes"),
        original
    );
    assert_eq!(path_from_native(&root_listing.target), PathBuf::new());
    assert!(matches!(empty_read, FileError::Invalid(_)));
    assert!(matches!(empty_create, FileError::Invalid(_)));
}

/// Public bounds reject oversized requests and report truncated listings.
#[test]
fn reads_creations_and_listings_enforce_public_bounds() {
    // Given a file above the maximum read size and two entries for a short listing.
    let mut fixture = Fixture::new();
    let oversized = vec![b'x'; MAX_FILE_BYTES + 1];
    fs::write(fixture.workspace.join("large.bin"), &oversized).expect("write oversized fixture");
    fs::write(fixture.workspace.join("one"), b"1").expect("write first entry");
    fs::write(fixture.workspace.join("two"), b"2").expect("write second entry");
    let binding = fixture.binding();

    // When size and listing bounds are exceeded.
    let (oversized_read, invalid_limit, oversized_create, invalid_listing, short_listing) = {
        let mut broker = FileBroker::new(&mut fixture.control);
        let oversized_read = broker
            .read(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, "large.bin"),
                MAX_FILE_BYTES,
            )
            .expect_err("one extra byte exceeds the read bound");
        let invalid_limit = broker
            .read(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, "one"),
                MAX_FILE_BYTES + 1,
            )
            .expect_err("caller cannot raise the read maximum");
        let oversized_create = broker
            .create_file(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, "too-large.bin"),
                &oversized,
            )
            .expect_err("caller cannot create an oversized file");
        let invalid_listing = broker
            .list(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, ""),
                0,
            )
            .expect_err("listing limit must be positive");
        let short_listing = broker
            .list(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, ""),
                1,
            )
            .expect("one-entry listing");
        (
            oversized_read,
            invalid_limit,
            oversized_create,
            invalid_listing,
            short_listing,
        )
    };

    // Then bounded operations return controlled failures and never create the oversized target.
    assert!(matches!(oversized_read, FileError::Native(_)));
    assert!(matches!(invalid_limit, FileError::Invalid(_)));
    assert!(matches!(oversized_create, FileError::Invalid(_)));
    assert!(matches!(invalid_listing, FileError::Invalid(_)));
    assert_eq!(short_listing.entries.len(), 1);
    assert!(short_listing.truncated);
    assert!(!fixture.workspace.join("too-large.bin").exists());
}

/// Native Unix names retain bytes that are not valid UTF-8.
#[cfg(unix)]
#[test]
fn non_unicode_names_round_trip_through_reads_and_listings() {
    use std::os::unix::ffi::OsStringExt;

    // Given a relative native filename containing a non-UTF-8 byte.
    let mut fixture = Fixture::new();
    let name = std::ffi::OsString::from_vec(b"native-\xff-name".to_vec());
    let relative = PathBuf::from(name.clone());
    fs::write(fixture.workspace.join(&relative), b"native bytes")
        .expect("write non-Unicode filename");
    let binding = fixture.binding();

    // When the broker reads the file and lists its parent directory.
    let (read, listing) = {
        let mut broker = FileBroker::new(&mut fixture.control);
        let read = broker
            .read(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, relative.clone()),
                100,
            )
            .expect("read native path");
        let listing = broker
            .list(
                &binding,
                &target(fixture.session_id, fixture.workspace_id, ""),
                10,
            )
            .expect("list native names");
        (read, listing)
    };

    // Then both returned path representations preserve the original byte sequence.
    assert_eq!(read.bytes, b"native bytes");
    assert_eq!(path_from_native(&read.target), relative);
    assert!(listing.entries.iter().any(|entry| {
        path_from_native(&entry.name).as_os_str() == name.as_os_str() && entry.kind == "file"
    }));
}

/// Run FIFO access in a child so a regression cannot hang the test process.
#[cfg(target_os = "linux")]
#[test]
fn fifo_read_fails_within_a_process_deadline() {
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    const CHILD_MARKER: &str = "ORANGECODE_FIFO_BROKER_CHILD";
    const DEADLINE: Duration = Duration::from_secs(5);
    const POLL: Duration = Duration::from_millis(10);

    if std::env::var_os(CHILD_MARKER).is_some() {
        let mut fixture = Fixture::new();
        let fifo = fixture.workspace.join("reader-fifo");
        let status = Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("create FIFO fixture");
        assert!(status.success(), "mkfifo must create the fixture");
        let binding = fixture.binding();
        let mut broker = FileBroker::new(&mut fixture.control);
        let result = broker.read(
            &binding,
            &target(fixture.session_id, fixture.workspace_id, "reader-fifo"),
            10,
        );
        assert!(matches!(result, Err(FileError::Native(_))));
        return;
    }

    let executable = std::env::current_exe().expect("locate integration test executable");
    let mut child = Command::new(executable)
        .arg("--exact")
        .arg("fifo_read_fails_within_a_process_deadline")
        .env(CHILD_MARKER, "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start bounded FIFO child");
    let deadline = Instant::now() + DEADLINE;
    loop {
        match child.try_wait().expect("check FIFO child status") {
            Some(status) => {
                assert!(status.success(), "FIFO child must reject the path cleanly");
                break;
            }
            None if Instant::now() < deadline => thread::sleep(POLL),
            None => {
                child.kill().expect("terminate hung FIFO child");
                let _ = child.wait().expect("reap FIFO child");
                panic!("FIFO read exceeded the five-second deadline");
            }
        }
    }
}
