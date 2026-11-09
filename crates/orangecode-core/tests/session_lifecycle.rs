use std::fs;
use std::path::Path;

use orangecode_core::session::{MessageRole, RequestManifest, SessionError, SessionJournal};
use tempfile::TempDir;
use uuid::Uuid;

fn setup() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = TempDir::new().expect("temporary directory");
    let root = temp.path().join("app-data");
    let workspace = temp.path().join("workspace-雪");
    fs::create_dir_all(&workspace).expect("workspace directory");
    (temp, root, workspace)
}

fn create(root: &Path, workspace: &Path) -> SessionJournal {
    SessionJournal::create(root, workspace, "Overview").expect("create session journal")
}

fn manifest() -> RequestManifest {
    RequestManifest {
        base_prompt: "base instructions".into(),
        prompt_version: "1".into(),
        prompt_sha256: "a".repeat(64),
        custom_instructions: "custom".into(),
        model: "local-model".into(),
        base_url: "http://127.0.0.1:1234/v1".into(),
        context_tokens: 32_000,
        max_output_tokens: 2_000,
        tool_schema_version: None,
        tools: None,
    }
}

#[test]
fn creation_writes_a_versioned_jsonl_file_under_utc_date_tree() {
    let (_temp, root, workspace) = setup();

    let journal = create(&root, &workspace);

    let relative = journal
        .path()
        .strip_prefix(&root)
        .expect("journal under data root");
    let parts: Vec<_> = relative.components().collect();
    assert_eq!(parts.len(), 5);
    assert_eq!(parts[0].as_os_str(), "sessions");
    assert_eq!(parts[1].as_os_str().to_string_lossy().len(), 4);
    assert_eq!(parts[2].as_os_str().to_string_lossy().len(), 2);
    assert_eq!(parts[3].as_os_str().to_string_lossy().len(), 2);
    let name = parts[4].as_os_str().to_string_lossy();
    assert!(name.starts_with("orangecode-"));
    assert!(name.ends_with(".jsonl"));
    assert_eq!(journal.snapshot().event_count, 1);
    let path = journal.path().to_path_buf();
    let workspace_id = journal.snapshot().workspace_id;
    assert!(!workspace_id.is_nil());
    drop(journal);
    let bytes = fs::read(&path).expect("journal bytes");
    assert_eq!(bytes.last(), Some(&b'\n'));
    let first: serde_json::Value =
        serde_json::from_slice(&bytes[..bytes.len() - 1]).expect("event JSON");
    assert_eq!(first["schema_version"], 2);
    assert_eq!(first["type"], "session.started");
    assert_eq!(first["seq"], 0);
    assert_eq!(first["payload"]["title"], "Overview");
    assert_eq!(first["payload"]["app_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(first["payload"]["workspace_id"], workspace_id.to_string());
    #[cfg(windows)]
    assert_eq!(first["payload"]["workspace"]["encoding"], "windows_utf16");
    #[cfg(unix)]
    assert_eq!(first["payload"]["workspace"]["encoding"], "unix_bytes");
}

#[test]
fn creation_rejects_a_file_as_workspace_before_creating_app_data() {
    let temporary = TempDir::new().expect("temporary directory");
    let data_root = temporary.path().join("app-data");
    let workspace_file = temporary.path().join("workspace-file");
    fs::write(&workspace_file, "not a directory").expect("workspace file");

    let error = SessionJournal::create(&data_root, &workspace_file, "Invalid workspace")
        .expect_err("workspace file must be rejected");

    assert!(matches!(error, SessionError::InvalidValue(_)));
    assert!(!data_root.exists());
}

#[test]
fn messages_and_request_manifest_survive_reopen_without_replaying_work() {
    let (_temp, root, workspace) = setup();
    let mut journal = create(&root, &workspace);
    journal
        .record_message(MessageRole::User, "雪\nnew line")
        .expect("record message");
    let request_id = journal.start_turn().expect("start turn");
    journal
        .record_manifest(request_id, manifest())
        .expect("record manifest");
    journal.complete_turn(request_id).expect("complete turn");
    journal
        .record_message(MessageRole::Assistant, "assistant response")
        .expect("record assistant message");
    let path = journal.path().to_path_buf();
    drop(journal);

    let reopened = SessionJournal::open(&path).expect("reopen journal");

    assert_eq!(reopened.snapshot().messages.len(), 2);
    assert_eq!(reopened.snapshot().messages[0].text, "雪\nnew line");
    assert_eq!(reopened.snapshot().messages[1].text, "assistant response");
    assert_eq!(reopened.snapshot().active_turn, None);
    assert_eq!(reopened.snapshot().event_count, 6);
    drop(reopened);
    let transcript = fs::read_to_string(path).expect("journal text");
    assert!(transcript.contains("request.manifest"));
    assert!(transcript.contains("base instructions"));
    assert!(transcript.contains("user_cli"));
    assert!(transcript.contains("provider"));
}

#[test]
fn a_second_writer_is_denied_while_the_first_holds_the_lock() {
    let (_temp, root, workspace) = setup();
    let journal = create(&root, &workspace);

    let error = SessionJournal::open(journal.path()).expect_err("second writer must fail");

    assert!(matches!(error, SessionError::Busy));
}

#[test]
fn archive_restore_are_idempotent_and_never_resume_a_turn() {
    let (_temp, root, workspace) = setup();
    let mut journal = create(&root, &workspace);
    journal.archive().expect("archive idle session");
    let archived_count = journal.snapshot().event_count;
    journal.archive().expect("repeat archive");
    assert_eq!(journal.snapshot().event_count, archived_count);
    assert!(journal.snapshot().archived);
    let path = journal.path().to_path_buf();
    drop(journal);

    let mut reopened = SessionJournal::open(&path).expect("open archived journal");
    assert!(reopened.snapshot().archived);
    assert_eq!(reopened.snapshot().active_turn, None);
    reopened.restore().expect("restore session");
    let restored_count = reopened.snapshot().event_count;
    reopened.restore().expect("repeat restore");
    assert_eq!(reopened.snapshot().event_count, restored_count);
    assert!(!reopened.snapshot().archived);
    assert!(!reopened.start_turn().expect("new explicit turn").is_nil());
}

#[test]
fn archive_refuses_an_active_turn_without_changing_the_journal() {
    let (_temp, root, workspace) = setup();
    let mut journal = create(&root, &workspace);
    let request_id = journal.start_turn().expect("start turn");
    let event_count = journal.snapshot().event_count;

    let error = journal
        .archive()
        .expect_err("active session cannot archive");

    assert!(matches!(error, SessionError::ActiveTurn));
    assert_eq!(journal.snapshot().event_count, event_count);
    assert!(!journal.snapshot().archived);
    journal
        .complete_turn(request_id)
        .expect("complete active turn");
}

#[test]
fn invalid_or_incomplete_journal_bytes_are_rejected_without_repair() {
    let (_temp, root, workspace) = setup();
    let journal = create(&root, &workspace);
    let path = journal.path().to_path_buf();
    drop(journal);
    let original = fs::read(&path).expect("initial journal");
    for suffix in [b"{\"broken\":".as_slice(), b"\nnot-json\n", b"\n"] {
        let mut candidate = original.clone();
        candidate.extend_from_slice(suffix);
        fs::write(&path, &candidate).expect("write malformed candidate");
        let error = SessionJournal::open(&path).expect_err("malformed journal must fail");
        assert!(matches!(error, SessionError::InvalidJournal(_)));
        assert_eq!(fs::read(&path).expect("read unchanged journal"), candidate);
    }
    let mut oversized = original;
    oversized.resize(8 * 1024 * 1024 + 1, b'x');
    fs::write(&path, &oversized).expect("write oversized candidate");
    assert!(SessionJournal::open(&path).is_err());
    assert_eq!(
        fs::metadata(&path).expect("metadata").len(),
        (8 * 1024 * 1024 + 1) as u64
    );
}

#[test]
fn malformed_schema_events_and_injected_roles_are_rejected_without_repair() {
    let (_temp, root, workspace) = setup();
    let journal = create(&root, &workspace);
    let path = journal.path().to_path_buf();
    let session_id = journal.snapshot().session_id;
    drop(journal);
    let initial = fs::read(&path).expect("initial journal bytes");
    let initial_event: serde_json::Value =
        serde_json::from_slice(&initial[..initial.len() - 1]).expect("initial event");
    let initial_event_id = initial_event["event_id"]
        .as_str()
        .expect("initial event UUID")
        .to_owned();
    let valid_message = |schema_version, event_id: String, seq, event_type, role| {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": schema_version,
            "session_id": session_id,
            "event_id": event_id,
            "seq": seq,
            "timestamp": "2026-11-08T00:00:00Z",
            "type": event_type,
            "payload": {"role": role, "source": "user_cli", "text": "x"}
        }))
        .expect("serialize candidate event")
    };
    let duplicate_key = format!(
        "{{\"schema_version\":1,\"schema_version\":1,\"session_id\":\"{session_id}\",\"event_id\":\"{}\",\"seq\":1,\"timestamp\":\"2026-11-08T00:00:00Z\",\"type\":\"message.recorded\",\"payload\":{{\"role\":\"user\",\"source\":\"user_cli\",\"text\":\"x\"}}}}",
        Uuid::new_v4()
    )
    .into_bytes();
    let candidates = [
        duplicate_key,
        valid_message(
            1,
            Uuid::new_v4().to_string(),
            1,
            "message.recorded",
            "system",
        ),
        valid_message(1, Uuid::new_v4().to_string(), 1, "message.recorded", "user"),
        valid_message(3, Uuid::new_v4().to_string(), 1, "message.recorded", "user"),
        valid_message(2, Uuid::new_v4().to_string(), 1, "future.event", "user"),
        valid_message(2, initial_event_id, 1, "message.recorded", "user"),
        valid_message(2, Uuid::new_v4().to_string(), 2, "message.recorded", "user"),
    ];

    for line in candidates {
        let mut candidate = initial.clone();
        candidate.extend_from_slice(&line);
        candidate.push(b'\n');
        fs::write(&path, &candidate).expect("write malformed candidate");
        assert!(matches!(
            SessionJournal::open(&path),
            Err(SessionError::InvalidJournal(_))
        ));
        assert_eq!(
            fs::read(&path).expect("journal unchanged after rejected replay"),
            candidate
        );
    }
}

#[test]
fn replay_rejects_message_source_that_conflicts_with_role() {
    let (_temp, root, workspace) = setup();
    let mut journal = create(&root, &workspace);
    journal
        .record_message(MessageRole::User, "user text")
        .expect("record user message");
    let path = journal.path().to_path_buf();
    drop(journal);

    let contents = fs::read_to_string(&path).expect("journal text");
    let tampered = contents.replace("\"source\":\"user_cli\"", "\"source\":\"provider\"");
    assert_ne!(tampered, contents);
    fs::write(&path, tampered).expect("write mismatched provenance");

    assert!(matches!(
        SessionJournal::open(&path),
        Err(SessionError::InvalidJournal(_))
    ));
}

#[test]
fn hard_linked_journal_handles_are_rejected() {
    let (_temp, root, workspace) = setup();
    let journal = create(&root, &workspace);
    let path = journal.path().to_path_buf();
    let session_id = journal.snapshot().session_id;
    let file_name = path.file_name().expect("journal filename");
    let alias_directory = root.join("hard-link-alias");
    fs::create_dir(&alias_directory).expect("hard link alias directory");
    let hardlink = alias_directory.join(file_name);
    assert!(
        hardlink
            .file_name()
            .expect("alias journal filename")
            .to_string_lossy()
            .contains(&session_id.to_string())
    );
    drop(journal);

    fs::hard_link(&path, &hardlink).expect("create hard link");
    assert!(matches!(
        SessionJournal::open(&hardlink),
        Err(SessionError::InvalidJournal(_))
    ));
    assert!(matches!(
        SessionJournal::open(&path),
        Err(SessionError::InvalidJournal(_))
    ));
}

#[cfg(windows)]
#[test]
fn windows_journals_deny_renaming_during_creation_and_reopen() {
    let (_temporary, root, workspace) = setup();
    let journal = create(&root, &workspace);
    let path = journal.path().to_path_buf();
    let renamed = path.with_extension("renamed");
    assert!(
        fs::rename(&path, &renamed).is_err(),
        "created writer must deny delete sharing"
    );
    drop(journal);
    let reopened = SessionJournal::open(&path).expect("reopen");
    assert!(
        fs::rename(&path, &renamed).is_err(),
        "reopened writer must deny delete sharing"
    );
    drop(reopened);
    fs::rename(&path, &renamed).expect("rename after ownership ends");
}

/// Construct a valid pre-reservation journal without thousands of sync calls.
fn legacy_full_journal(byte_limit: bool) -> (TempDir, std::path::PathBuf) {
    let (temporary, root, workspace) = setup();
    let journal = create(&root, &workspace);
    let path = journal.path().to_path_buf();
    drop(journal);
    let mut first: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("initial record")).expect("first record");
    // Model a schema-v1 journal from before the turn/tool schema increment.
    first["schema_version"] = serde_json::json!(1);
    let mut bytes = serde_json::to_vec(&first).expect("v1 first record");
    bytes.push(b'\n');
    let session_id = first["session_id"].clone();
    let request_id = Uuid::new_v4();
    let mut seq = 1_u64;
    let event = |seq, kind, payload| {
        serde_json::json!({
            "schema_version":1,"session_id":session_id,"event_id":Uuid::new_v4(),
            "seq":seq,"timestamp":"2026-11-08T00:00:00Z","type":kind,"payload":payload
        })
    };
    let mut append = |value: serde_json::Value| {
        bytes.extend(serde_json::to_vec(&value).expect("event"));
        bytes.push(b'\n');
    };
    append(event(
        seq,
        "turn.started",
        serde_json::json!({"request_id":request_id}),
    ));
    seq += 1;
    if byte_limit {
        loop {
            let mut value = event(
                seq,
                "message.recorded",
                serde_json::json!({"role":"user","source":"user_cli","text":""}),
            );
            let overhead = serde_json::to_vec(&value).expect("event").len() + 1;
            let remaining = 8 * 1024 * 1024 - bytes.len();
            if remaining < overhead {
                break;
            }
            value["payload"]["text"] =
                serde_json::Value::String("x".repeat((remaining - overhead).min(16 * 1024)));
            bytes.extend(serde_json::to_vec(&value).expect("event"));
            bytes.push(b'\n');
            seq += 1;
        }
        assert_eq!(bytes.len(), 8 * 1024 * 1024);
    } else {
        while seq < 10_000 {
            let value = event(
                seq,
                "message.recorded",
                serde_json::json!({"role":"user","source":"user_cli","text":""}),
            );
            bytes.extend(serde_json::to_vec(&value).expect("event"));
            bytes.push(b'\n');
            seq += 1;
        }
    }
    fs::write(&path, bytes).expect("legacy full journal");
    (temporary, path)
}

#[test]
fn legacy_full_active_journals_allow_one_bounded_terminal_recovery() {
    for byte_limit in [false, true] {
        let (_temporary, path) = legacy_full_journal(byte_limit);
        let mut journal = SessionJournal::open(&path).expect("open full legacy journal");
        let request_id = journal.snapshot().active_turn.expect("active legacy turn");
        journal
            .fail_turn(request_id, "operator_interrupted")
            .expect("recover full legacy turn");
        assert_eq!(journal.snapshot().active_turn, None);
        assert!(matches!(
            journal.record_message(MessageRole::User, "no ordinary overflow"),
            Err(SessionError::LimitReached)
        ));
        drop(journal);
        assert_eq!(
            SessionJournal::open(&path)
                .expect("reopen recovered journal")
                .snapshot()
                .active_turn,
            None
        );
        let mut bytes = fs::read(&path).expect("recovered bytes");
        let first: serde_json::Value = serde_json::from_slice(
            bytes
                .split(|byte| *byte == b'\n')
                .next()
                .expect("first line"),
        )
        .expect("first record");
        let extra = serde_json::json!({"schema_version":1,"session_id":first["session_id"],
            "event_id":Uuid::new_v4(),"seq":bytes.iter().filter(|byte| **byte == b'\n').count(),
            "timestamp":"2026-11-08T00:00:00Z","type":"message.recorded",
            "payload":{"role":"user","source":"user_cli","text":"forged overflow"}});
        bytes.extend(serde_json::to_vec(&extra).expect("extra record"));
        bytes.push(b'\n');
        fs::write(&path, &bytes).expect("import ordinary overflow");
        assert!(
            matches!(
                SessionJournal::open(&path),
                Err(SessionError::InvalidJournal(_))
            ),
            "reader must also deny ordinary overflow"
        );
        assert_eq!(fs::read(&path).expect("unchanged invalid import"), bytes);
    }
}

#[cfg(unix)]
#[test]
fn symbolic_link_journal_paths_are_rejected() {
    use std::os::unix::fs::symlink;

    let (_temp, root, workspace) = setup();
    let journal = create(&root, &workspace);
    let path = journal.path().to_path_buf();
    let file_name = path.file_name().expect("journal filename");
    let alias_directory = root.join("symbolic-link-alias");
    fs::create_dir(&alias_directory).expect("symlink alias directory");
    let alias = alias_directory.join(file_name);
    drop(journal);
    symlink(&path, &alias).expect("create symlink");
    assert!(matches!(
        SessionJournal::open(&alias),
        Err(SessionError::InvalidJournal(_))
    ));
}
