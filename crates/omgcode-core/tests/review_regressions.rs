use std::fs;
use std::path::PathBuf;

use omgcode_core::control::ControlStore;
use omgcode_core::files::{FileBroker, FileError, FileTarget};
use omgcode_core::policy::{DecisionReason, PermissionSet, SessionBinding};
use serde_json::Value;
use tempfile::TempDir;
use uuid::Uuid;

const OPERATION_EVENT_CAP: usize = 4096;

struct Fixture {
    data: PathBuf,
    root: PathBuf,
    control: Option<ControlStore>,
    binding: SessionBinding,
    target: FileTarget,
    _temporary: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("private temporary fixture");
        let data = temporary.path().join("application-data");
        let root = temporary.path().join("development-root");
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("create private workspace");
        fs::write(workspace.join("read.txt"), b"bounded read fixture")
            .expect("write private read fixture");
        let mut control = ControlStore::open(&data).expect("open private settings");
        control
            .set_development_root(&root, control.revision())
            .expect("select private development root");
        let registration = control
            .register_workspace(&workspace, control.revision())
            .expect("register private workspace");
        let session_id = Uuid::new_v4();
        let binding = control
            .bind_session(session_id, registration.id())
            .expect("bind current workspace");
        let target = FileTarget {
            session_id,
            workspace_id: registration.id(),
            path: PathBuf::from("read.txt"),
        };
        Self {
            _temporary: temporary,
            data,
            root,
            control: Some(control),
            binding,
            target,
        }
    }

    fn control(&mut self) -> &mut ControlStore {
        self.control
            .as_mut()
            .expect("fixture control store is open")
    }

    fn settings_bytes(&mut self) -> Vec<u8> {
        // Windows enforces the writer's byte-range lock against separate reads.
        // Release that writer, inspect its exact bytes, then reopen the same
        // existing store before the next operation. Never recreate the journal.
        drop(self.control.take());
        let bytes = fs::read(self.data.join("control-v1.jsonl"))
            .expect("read exact settings bytes after closing the writer");
        self.control = Some(
            ControlStore::open_existing(&self.data)
                .expect("reopen the same settings journal")
                .expect("the settings journal must still exist"),
        );
        bytes
    }

    fn read(&mut self) -> Result<Vec<u8>, FileError> {
        FileBroker::new(
            self.control
                .as_mut()
                .expect("fixture control store is open"),
        )
        .read(&self.binding, &self.target, 100)
        .map(|read| read.bytes)
    }

    // Build a valid capped log in memory from real serialized events. The broker
    // and its journal handle have dropped before this single fixture write.
    fn fill_operation_journal(&self) {
        let path = self.data.join("file-operations-v1.jsonl");
        let text = fs::read_to_string(&path).expect("read completed operation templates");
        let entries = text
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("valid generated JSON event"))
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 3, "one Created and one completed read");
        assert_eq!(entries[0]["action"]["type"], "created");
        assert_eq!(entries[1]["action"]["type"], "operation_started");
        assert_eq!(entries[2]["action"]["type"], "operation_finished");
        let mut capped = vec![entries[0].clone()];
        let mut append = |mut event: Value, operation_id: Uuid| {
            event["event_id"] = Value::String(Uuid::new_v4().to_string());
            event["sequence"] =
                Value::from(u64::try_from(capped.len()).expect("bounded event count"));
            event["action"]["payload"]["operation_id"] = Value::String(operation_id.to_string());
            capped.push(event);
        };

        // Created plus one acknowledged uncertainty uses four events. Completed
        // pairs then reach the even cap without leaving a pending operation.
        let first_id = Uuid::new_v4();
        append(entries[1].clone(), first_id);
        let mut uncertain = entries[2].clone();
        uncertain["action"]["payload"]["outcome"] = Value::String("uncertain".into());
        append(uncertain, first_id);
        let mut acknowledged = entries[2].clone();
        acknowledged["action"] = serde_json::json!({
            "type": "operation_acknowledged",
            "payload": { "operation_id": first_id }
        });
        append(acknowledged, first_id);
        for _ in 0..((OPERATION_EVENT_CAP - 4) / 2) {
            let operation_id = Uuid::new_v4();
            append(entries[1].clone(), operation_id);
            append(entries[2].clone(), operation_id);
        }
        assert_eq!(capped.len(), OPERATION_EVENT_CAP);
        let mut encoded = Vec::new();
        for entry in capped {
            serde_json::to_writer(&mut encoded, &entry).expect("encode strict fixture event");
            encoded.push(b'\n');
        }
        fs::write(path, encoded).expect("write complete capped operation journal once");
    }
}

#[test]
fn a_successful_file_read_does_not_append_to_the_settings_journal() {
    let mut fixture = Fixture::new();
    let before = fixture.settings_bytes();

    let bytes = fixture.read().expect("read authorized private file");

    assert_eq!(bytes, b"bounded read fixture");
    assert!(
        fixture.settings_bytes() == before,
        "settings journal bytes must remain unchanged"
    );
}

#[test]
fn a_full_operation_journal_does_not_grow_settings_or_prevent_permission_revocation() {
    let mut fixture = Fixture::new();
    fixture
        .read()
        .expect("create real completed operation templates");
    fixture.fill_operation_journal();
    let before = fixture.settings_bytes();
    let revision = fixture.control().revision();

    // Strict public replay proves that the generated cap is valid and complete.
    let audit = FileBroker::new(fixture.control())
        .audit(100)
        .expect("strictly replay capped operation journal");
    assert_eq!(audit.pending, None);
    assert_eq!(audit.entries.len(), 100);
    assert_eq!(
        audit.entries.last().expect("last capped event")["sequence"],
        4095
    );
    for _ in 0..3 {
        let error = fixture
            .read()
            .expect_err("the event cap must refuse each read");
        assert!(
            matches!(&error, FileError::Audit(source) if source.to_string().contains("capacity reached")),
            "expected an operation-journal capacity refusal, got {error}"
        );
    }

    assert!(
        fixture.settings_bytes() == before,
        "settings journal bytes must remain unchanged"
    );
    assert_eq!(fixture.control().revision(), revision);
    let workspace_id = fixture.target.workspace_id;
    let session_id = fixture.target.session_id;
    fixture
        .control()
        .set_permissions(
            workspace_id,
            PermissionSet {
                read: false,
                ..PermissionSet::default()
            },
            revision,
        )
        .expect("a full operation journal must not block permission revocation");
    assert_eq!(fixture.control().revision(), revision + 1);
    fixture.binding = fixture
        .control()
        .bind_session(session_id, workspace_id)
        .expect("bind current revoked permissions");
    assert!(matches!(
        fixture.read(),
        Err(FileError::Denied(DecisionReason::PermissionDisabled))
    ));
}

#[test]
fn registration_refuses_a_workspace_with_33_components_without_appending_settings() {
    let mut fixture = Fixture::new();
    let mut deep_workspace = fixture.root.clone();
    for _ in 0..33 {
        deep_workspace.push("d");
    }
    fs::create_dir_all(&deep_workspace).expect("create over-depth private workspace");
    let before = fixture.settings_bytes();
    let revision = fixture.control().revision();

    let error = match fixture
        .control()
        .register_workspace(&deep_workspace, revision)
    {
        Err(error) => error,
        Ok(_) => panic!("registration must reject an unusable workspace path"),
    };

    assert!(
        error.to_string().contains("workspace path"),
        "useful path error: {error}"
    );
    assert!(
        fixture.settings_bytes() == before,
        "settings journal bytes must remain unchanged"
    );
    assert_eq!(fixture.control().revision(), revision);
}
