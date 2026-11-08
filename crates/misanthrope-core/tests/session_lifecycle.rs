use std::fs;
use std::path::Path;

use misanthrope_core::session::{MessageRole, RequestManifest, SessionError, SessionJournal};
use tempfile::TempDir;
use uuid::Uuid;

fn setup() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = TempDir::new().expect("temporary directory");
    let root = temp.path().join("app-data");
    let workspace = temp.path().join("workspace-雪\nline");
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
    assert!(name.starts_with("misanthrope-"));
    assert!(name.ends_with(".jsonl"));
    assert_eq!(journal.snapshot().event_count, 1);
    let bytes = fs::read(journal.path()).expect("journal bytes");
    assert_eq!(bytes.last(), Some(&b'\n'));
    let first: serde_json::Value =
        serde_json::from_slice(&bytes[..bytes.len() - 1]).expect("event JSON");
    assert_eq!(first["schema_version"], 1);
    assert_eq!(first["type"], "session.started");
    assert_eq!(first["seq"], 0);
    assert_eq!(first["payload"]["title"], "Overview");
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
    let path = journal.path().to_path_buf();
    drop(journal);

    let reopened = SessionJournal::open(&path).expect("reopen journal");

    assert_eq!(reopened.snapshot().messages.len(), 1);
    assert_eq!(reopened.snapshot().messages[0].text, "雪\nnew line");
    assert_eq!(reopened.snapshot().active_turn, None);
    assert_eq!(reopened.snapshot().event_count, 5);
    let transcript = fs::read_to_string(path).expect("journal text");
    assert!(transcript.contains("request.manifest"));
    assert!(transcript.contains("base instructions"));
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
    assert_eq!(
        reopened.start_turn().expect("new explicit turn").is_nil(),
        false
    );
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
fn duplicate_json_keys_and_non_user_roles_are_rejected() {
    let (_temp, root, workspace) = setup();
    let journal = create(&root, &workspace);
    let path = journal.path().to_path_buf();
    let session_id = journal.snapshot().session_id;
    drop(journal);
    let duplicate = format!(
        "{{\"schema_version\":1,\"schema_version\":1,\"session_id\":\"{session_id}\",\"event_id\":\"{}\",\"seq\":1,\"timestamp\":\"2026-11-08T00:00:00Z\",\"type\":\"message.recorded\",\"payload\":{{\"role\":\"user\",\"text\":\"x\"}}}}\n",
        Uuid::new_v4()
    );
    let mut bytes = fs::read(&path).expect("initial bytes");
    bytes.extend_from_slice(duplicate.as_bytes());
    fs::write(&path, &bytes).expect("write duplicate key");
    assert!(matches!(
        SessionJournal::open(&path),
        Err(SessionError::InvalidJournal(_))
    ));

    let mut bytes = fs::read(&path).expect("duplicate bytes");
    bytes.truncate(
        bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .expect("newline"),
    );
    bytes.extend_from_slice(format!(
        "{{\"schema_version\":1,\"session_id\":\"{session_id}\",\"event_id\":\"{}\",\"seq\":1,\"timestamp\":\"2026-11-08T00:00:00Z\",\"type\":\"message.recorded\",\"payload\":{{\"role\":\"system\",\"text\":\"x\"}}}}\n",
        Uuid::new_v4()
    ).as_bytes());
    fs::write(&path, &bytes).expect("write injected role");
    assert!(matches!(
        SessionJournal::open(&path),
        Err(SessionError::InvalidJournal(_))
    ));
}

#[cfg(unix)]
#[test]
fn linked_journal_paths_are_rejected() {
    use std::os::unix::fs::symlink;

    let (_temp, root, workspace) = setup();
    let journal = create(&root, &workspace);
    let path = journal.path().to_path_buf();
    let alias = path.with_file_name("alias.jsonl");
    drop(journal);
    symlink(&path, &alias).expect("create symlink");
    assert!(matches!(
        SessionJournal::open(&alias),
        Err(SessionError::InvalidJournal(_))
    ));

    let hardlink = path.with_file_name("hardlink.jsonl");
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
