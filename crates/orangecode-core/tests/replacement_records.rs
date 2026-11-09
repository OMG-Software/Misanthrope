//! Read-only replacement record validation through the public core API.

use std::path::Path;

use orangecode_core::replacement::{
    InitializationPhase, ReplacementError, ReplacementOutcome, ReplacementProgress,
    ReplacementScope, inspect_initialization, inspect_ledger,
};
use orangecode_core::session::NativePath;
use serde_json::{Value, json};
use uuid::Uuid;

const MAX_RECORD: usize = 64 * 1024;
const MAX_LEDGER: usize = 8 * 1024 * 1024;

struct Fixture {
    control: Uuid,
    store: Uuid,
    operation: Uuid,
    events: Vec<Value>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            control: Uuid::new_v4(),
            store: Uuid::new_v4(),
            operation: Uuid::new_v4(),
            events: Vec::new(),
        }
    }

    fn scope(&self) -> ReplacementScope {
        ReplacementScope::new(self.control, self.store).expect("valid expected scope")
    }

    fn push(&mut self, kind: &str, payload: Option<Value>) {
        let action = match payload {
            Some(payload) => json!({"type": kind, "payload": payload}),
            None => json!({"type": kind}),
        };
        self.events.push(json!({
            "schema_version": 1,
            "control_store_id": self.control,
            "replacement_store_id": self.store,
            "event_id": Uuid::new_v4(),
            "sequence": self.events.len(),
            "timestamp": "2026-11-09T16:00:00Z",
            "action": action,
        }));
    }

    fn start(&mut self) {
        self.push(
            "replacement_started",
            Some(json!({
                "operation_id": self.operation,
                "session_id": Uuid::new_v4(),
                "workspace_id": Uuid::new_v4(),
                "revision": 3,
                "target": NativePath::from_path(Path::new("src/file.txt")).expect("native target"),
                "expected": {"bytes": 4, "sha256": "a".repeat(64)},
                "proposed": {"bytes": 5, "sha256": "b".repeat(64)},
                "artifacts": {
                    "original": "original", "proposed": "proposed",
                    "manifest": "manifest.json", "stage": "stage"
                }
            })),
        );
    }

    fn prepared(&mut self) {
        self.push(
            "checkpoint_prepared",
            Some(json!({
                "operation_id": self.operation,
                "manifest": {"bytes": 512, "sha256": "c".repeat(64)}
            })),
        );
    }

    fn staged(&mut self) {
        self.push(
            "stage_prepared",
            Some(json!({
                "operation_id": self.operation,
                "identity": {"volume": 7, "file_id": vec![9; 16]},
                "content": {"bytes": 5, "sha256": "b".repeat(64)}
            })),
        );
    }

    fn commit(&mut self) {
        self.push(
            "commit_intended",
            Some(json!({"operation_id": self.operation})),
        );
    }

    fn finish(&mut self, outcome: &str) {
        let mut payload = json!({"operation_id": self.operation, "outcome": outcome});
        if outcome != "succeeded" {
            payload["code"] = json!("test_failure");
        }
        self.push("replacement_finished", Some(payload));
    }

    fn acknowledge(&mut self) {
        self.push(
            "replacement_acknowledged",
            Some(json!({"operation_id": self.operation})),
        );
    }

    fn bytes(&self) -> Vec<u8> {
        encode(&self.events)
    }
}

fn encode(events: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for event in events {
        serde_json::to_writer(&mut bytes, event).expect("fixture JSON");
        bytes.push(b'\n');
    }
    bytes
}

fn ledger() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.push("created", None);
    fixture
}

#[test]
fn initialization_retains_incomplete_state_and_requires_matching_ready_record() {
    let mut fixture = Fixture::new();
    fixture.push("initializing", None);
    let incomplete =
        inspect_initialization(&fixture.bytes(), fixture.control).expect("valid prefix");
    assert_eq!(incomplete.phase, InitializationPhase::Initializing);
    assert_eq!(incomplete.event_count, 1);
    assert_eq!(incomplete.scope, fixture.scope());

    fixture.push("ready", None);
    let ready = inspect_initialization(&fixture.bytes(), fixture.control).expect("recorded ready");
    assert_eq!(ready.phase, InitializationPhase::Ready);
    assert_eq!(ready.event_count, 2);
    assert_eq!(ready.encoded_bytes, fixture.bytes().len());
    assert!(inspect_initialization(&fixture.bytes(), Uuid::new_v4()).is_err());

    fixture.events[1]["replacement_store_id"] = json!(Uuid::new_v4());
    assert!(inspect_initialization(&fixture.bytes(), fixture.control).is_err());
}

#[test]
fn initialization_rejects_empty_partial_reordered_or_extra_records() {
    let mut fixture = Fixture::new();
    fixture.push("initializing", None);
    let bytes = fixture.bytes();
    for invalid in [
        Vec::new(),
        b"\n".to_vec(),
        bytes[..bytes.len() - 1].to_vec(),
    ] {
        assert!(inspect_initialization(&invalid, fixture.control).is_err());
    }
    fixture.events[0]["action"]["type"] = json!("ready");
    assert!(inspect_initialization(&fixture.bytes(), fixture.control).is_err());
    fixture.events[0]["action"]["type"] = json!("initializing");
    fixture.push("ready", None);
    fixture.push("ready", None);
    assert!(matches!(
        inspect_initialization(&fixture.bytes(), fixture.control),
        Err(ReplacementError::Limit(_))
    ));
}

#[test]
fn every_unfinished_phase_remains_pending_without_synthesized_outcomes() {
    let mut fixture = ledger();
    let empty = inspect_ledger(&fixture.bytes(), fixture.scope()).expect("header only");
    assert!(empty.operations.is_empty());
    assert!(empty.pending().is_none());
    fixture.start();
    for progress in [
        ReplacementProgress::Intent,
        ReplacementProgress::CheckpointPrepared,
        ReplacementProgress::StagePrepared,
        ReplacementProgress::CommitIntended,
    ] {
        let bytes = fixture.bytes();
        let original = bytes.clone();
        let snapshot = inspect_ledger(&bytes, fixture.scope()).expect("valid unfinished state");
        let pending = snapshot.pending().expect("unresolved operation");
        assert_eq!(pending.operation_id, fixture.operation);
        assert_eq!(pending.progress, progress);
        assert_eq!(pending.outcome, None);
        assert!(!pending.acknowledged);
        assert_eq!(bytes, original);
        match progress {
            ReplacementProgress::Intent => fixture.prepared(),
            ReplacementProgress::CheckpointPrepared => fixture.staged(),
            ReplacementProgress::StagePrepared => fixture.commit(),
            ReplacementProgress::CommitIntended => {}
        }
    }
}

#[test]
fn successful_record_retains_exact_metadata_and_clears_pending_state() {
    let mut fixture = ledger();
    fixture.start();
    fixture.prepared();
    fixture.staged();
    fixture.commit();
    fixture.finish("succeeded");
    let bytes = fixture.bytes();
    let snapshot = inspect_ledger(&bytes, fixture.scope()).expect("complete lifecycle");
    assert_eq!(snapshot.scope, fixture.scope());
    assert_eq!(snapshot.event_count, 6);
    assert_eq!(snapshot.encoded_bytes, bytes.len());
    assert!(snapshot.pending().is_none());
    let record = snapshot.operations.first().expect("one operation");
    assert_eq!(
        record.target,
        NativePath::from_path(Path::new("src/file.txt")).expect("target")
    );
    assert_eq!(record.expected.bytes, 4);
    assert_eq!(record.expected.sha256, "a".repeat(64));
    assert_eq!(record.proposed.bytes, 5);
    assert_eq!(
        record.manifest.as_ref().expect("manifest reference").bytes,
        512
    );
    assert_eq!(
        record
            .stage_identity
            .as_ref()
            .expect("stage identity")
            .file_id,
        [9; 16]
    );
    assert_eq!(record.outcome, Some(ReplacementOutcome::Succeeded));
    assert!(!record.acknowledged);
}

#[test]
fn uncertainty_blocks_new_intents_until_acknowledgement_without_claiming_success() {
    let mut fixture = ledger();
    fixture.start();
    fixture.prepared();
    fixture.staged();
    fixture.commit();
    fixture.finish("uncertain");
    let snapshot = inspect_ledger(&fixture.bytes(), fixture.scope()).expect("uncertain outcome");
    assert_eq!(
        snapshot.pending().expect("pending").outcome,
        Some(ReplacementOutcome::Uncertain)
    );
    let mut parallel = fixture.events.clone();
    fixture.operation = Uuid::new_v4();
    fixture.start();
    assert!(inspect_ledger(&fixture.bytes(), fixture.scope()).is_err());
    fixture.events = std::mem::take(&mut parallel);
    fixture.operation = snapshot.operations[0].operation_id;
    fixture.acknowledge();
    let acknowledged =
        inspect_ledger(&fixture.bytes(), fixture.scope()).expect("explicit acknowledgement");
    assert!(acknowledged.pending().is_none());
    assert!(acknowledged.operations[0].acknowledged);
    assert_eq!(
        acknowledged.operations[0].outcome,
        Some(ReplacementOutcome::Uncertain)
    );
    fixture.operation = Uuid::new_v4();
    fixture.start();
    assert!(inspect_ledger(&fixture.bytes(), fixture.scope()).is_ok());
}

#[test]
fn acknowledgement_can_close_unfinished_preparation_but_never_creates_an_outcome() {
    let mut fixture = ledger();
    fixture.start();
    fixture.acknowledge();
    let snapshot = inspect_ledger(&fixture.bytes(), fixture.scope()).expect("acknowledged intent");
    assert!(snapshot.pending().is_none());
    assert_eq!(snapshot.operations[0].outcome, None);
    assert_eq!(snapshot.operations[0].progress, ReplacementProgress::Intent);
    assert!(snapshot.operations[0].acknowledged);
    fixture.acknowledge();
    assert!(inspect_ledger(&fixture.bytes(), fixture.scope()).is_err());
}

#[test]
fn clean_failure_and_conflict_are_pre_commit_only_and_success_requires_commit() {
    for outcome in ["failed", "conflict"] {
        let mut fixture = ledger();
        fixture.start();
        fixture.finish(outcome);
        assert!(
            inspect_ledger(&fixture.bytes(), fixture.scope())
                .expect("precommit refusal")
                .pending()
                .is_none()
        );
        let mut committed = ledger();
        committed.start();
        committed.prepared();
        committed.staged();
        committed.commit();
        committed.finish(outcome);
        assert!(inspect_ledger(&committed.bytes(), committed.scope()).is_err());
    }
    let mut early = ledger();
    early.start();
    early.finish("succeeded");
    assert!(inspect_ledger(&early.bytes(), early.scope()).is_err());
}

#[test]
fn ordered_transitions_and_matching_operation_ids_are_required() {
    for kind in [
        "checkpoint_prepared",
        "stage_prepared",
        "commit_intended",
        "replacement_finished",
        "replacement_acknowledged",
    ] {
        let mut fixture = ledger();
        fixture.start();
        fixture.prepared();
        fixture.staged();
        fixture.commit();
        fixture.finish("uncertain");
        fixture.acknowledge();
        let action = fixture
            .events
            .iter_mut()
            .find(|event| event["action"]["type"] == kind)
            .expect("event");
        action["action"]["payload"]["operation_id"] = json!(Uuid::new_v4());
        assert!(
            inspect_ledger(&fixture.bytes(), fixture.scope()).is_err(),
            "{kind}"
        );
    }
    let mut fixture = ledger();
    fixture.start();
    fixture.commit();
    assert!(inspect_ledger(&fixture.bytes(), fixture.scope()).is_err());
    let mut fixture = ledger();
    fixture.start();
    fixture.staged();
    assert!(inspect_ledger(&fixture.bytes(), fixture.scope()).is_err());
    let mut fixture = ledger();
    fixture.start();
    fixture.prepared();
    fixture.prepared();
    assert!(inspect_ledger(&fixture.bytes(), fixture.scope()).is_err());
}

#[test]
fn duplicate_keys_unknown_fields_and_untrusted_extra_json_are_rejected() {
    let mut fixture = ledger();
    fixture.start();
    let original = String::from_utf8(fixture.bytes()).expect("JSON UTF-8");
    for needle in [
        "\"schema_version\":1",
        "\"bytes\":4",
        "\"original\":\"original\"",
    ] {
        let duplicate = original.replacen(needle, &format!("{needle},{needle}"), 1);
        assert!(
            inspect_ledger(duplicate.as_bytes(), fixture.scope()).is_err(),
            "{needle}"
        );
    }
    for path in [
        vec!["unexpected"],
        vec!["action", "unexpected"],
        vec!["action", "payload", "unexpected"],
        vec!["action", "payload", "expected", "unexpected"],
        vec!["action", "payload", "target", "unexpected"],
    ] {
        let mut events = fixture.events.clone();
        let mut cursor = &mut events[1];
        for key in path {
            cursor = &mut cursor[key];
        }
        *cursor = json!(true);
        assert!(inspect_ledger(&encode(&events), fixture.scope()).is_err());
    }
    let mut trailing = fixture.bytes();
    trailing.extend_from_slice(b"{}\n");
    assert!(inspect_ledger(&trailing, fixture.scope()).is_err());
    let same_line = original.replacen('\n', " {}\n", 1);
    assert!(inspect_ledger(same_line.as_bytes(), fixture.scope()).is_err());
}

#[test]
fn envelope_scope_identifiers_sequence_timestamp_and_version_are_validated() {
    let fixture = ledger();
    for (key, value) in [
        ("schema_version", json!(2)),
        ("sequence", json!(1)),
        ("sequence", json!(u64::MAX)),
        ("control_store_id", json!(Uuid::new_v4())),
        ("replacement_store_id", json!(Uuid::new_v4())),
        ("event_id", json!(Uuid::nil())),
        ("timestamp", json!("2026-11-09T17:00:00+01:00")),
        ("timestamp", json!("invalid")),
    ] {
        let mut events = fixture.events.clone();
        events[0][key] = value;
        assert!(
            inspect_ledger(&encode(&events), fixture.scope()).is_err(),
            "{key}"
        );
    }
    assert!(ReplacementScope::new(Uuid::nil(), fixture.store).is_err());
    assert!(ReplacementScope::new(fixture.control, Uuid::nil()).is_err());
    assert!(inspect_initialization(b"{}\n", Uuid::nil()).is_err());
    let mut duplicate = ledger();
    duplicate.start();
    duplicate.events[1]["event_id"] = duplicate.events[0]["event_id"].clone();
    assert!(inspect_ledger(&duplicate.bytes(), duplicate.scope()).is_err());
    for key in ["operation_id", "session_id", "workspace_id"] {
        let mut invalid = ledger();
        invalid.start();
        invalid.events[1]["action"]["payload"][key] = json!(Uuid::nil());
        assert!(
            inspect_ledger(&invalid.bytes(), invalid.scope()).is_err(),
            "{key}"
        );
    }
}

#[test]
fn descriptors_paths_artifact_names_and_failure_codes_have_strict_bounds() {
    let fixture = {
        let mut f = ledger();
        f.start();
        f
    };
    for (key, value) in [
        ("bytes", json!(1_048_577)),
        ("sha256", json!("A".repeat(64))),
        ("sha256", json!("a".repeat(63))),
    ] {
        let mut events = fixture.events.clone();
        events[1]["action"]["payload"]["expected"][key] = value;
        assert!(inspect_ledger(&encode(&events), fixture.scope()).is_err());
    }
    for target in ["../secret", "/absolute", "", "dir//file", "a/./file"] {
        let mut events = fixture.events.clone();
        events[1]["action"]["payload"]["target"] =
            serde_json::to_value(NativePath::from_path(Path::new(target)).expect("encoded target"))
                .expect("target JSON");
        assert!(
            inspect_ledger(&encode(&events), fixture.scope()).is_err(),
            "{target}"
        );
    }
    let mut unsafe_name = fixture.events.clone();
    unsafe_name[1]["action"]["payload"]["artifacts"]["stage"] = json!("../stage");
    assert!(inspect_ledger(&encode(&unsafe_name), fixture.scope()).is_err());
    let mut prepared = fixture;
    prepared.prepared();
    for size in [0, 65_537] {
        let mut events = prepared.events.clone();
        events[2]["action"]["payload"]["manifest"]["bytes"] = json!(size);
        assert!(inspect_ledger(&encode(&events), prepared.scope()).is_err());
    }
    prepared.staged();
    prepared.events[3]["action"]["payload"]["content"]["bytes"] = json!(6);
    assert!(inspect_ledger(&prepared.bytes(), prepared.scope()).is_err());
    let mut failed = ledger();
    failed.start();
    failed.finish("failed");
    for code in ["", "has spaces", "secret\n", &"x".repeat(129)] {
        let mut events = failed.events.clone();
        events[2]["action"]["payload"]["code"] = json!(code);
        assert!(inspect_ledger(&encode(&events), failed.scope()).is_err());
    }
}

#[test]
fn operation_ids_and_policy_revisions_cannot_be_reused_or_rewound() {
    let mut fixture = ledger();
    fixture.start();
    fixture.finish("failed");
    fixture.start();
    assert!(inspect_ledger(&fixture.bytes(), fixture.scope()).is_err());
    fixture.events[3]["action"]["payload"]["operation_id"] = json!(Uuid::new_v4());
    fixture.events[3]["action"]["payload"]["revision"] = json!(2);
    assert!(inspect_ledger(&fixture.bytes(), fixture.scope()).is_err());
}

#[test]
fn record_size_includes_lf_and_crlf_and_initialization_has_its_own_byte_cap() {
    let fixture = ledger();
    let mut bytes = fixture.bytes();
    bytes.pop();
    bytes.resize(MAX_RECORD - 1, b' ');
    bytes.push(b'\n');
    assert!(inspect_ledger(&bytes, fixture.scope()).is_ok());
    bytes.insert(bytes.len() - 1, b'\r');
    assert!(matches!(
        inspect_ledger(&bytes, fixture.scope()),
        Err(ReplacementError::Limit(_))
    ));
    bytes.remove(bytes.len() - 3);
    assert!(inspect_ledger(&bytes, fixture.scope()).is_ok());
    let mut anchor = Fixture::new();
    anchor.push("initializing", None);
    let mut bytes = anchor.bytes();
    bytes.pop();
    bytes.resize(4095, b' ');
    bytes.push(b'\n');
    assert!(inspect_initialization(&bytes, anchor.control).is_ok());
    bytes.insert(bytes.len() - 1, b' ');
    assert!(matches!(
        inspect_initialization(&bytes, anchor.control),
        Err(ReplacementError::Limit(_))
    ));
}

#[test]
fn ledger_record_and_retained_operation_caps_do_not_evict_history() {
    let mut fixture = ledger();
    for _ in 0..64 {
        fixture.operation = Uuid::new_v4();
        fixture.start();
        fixture.finish("failed");
    }
    assert_eq!(
        inspect_ledger(&fixture.bytes(), fixture.scope())
            .expect("64 retained operations")
            .operations
            .len(),
        64
    );
    fixture.operation = Uuid::new_v4();
    fixture.start();
    assert!(matches!(
        inspect_ledger(&fixture.bytes(), fixture.scope()),
        Err(ReplacementError::Limit(_))
    ));
    let header = ledger();
    let many_records = header.bytes().repeat(4097);
    assert!(matches!(
        inspect_ledger(&many_records, header.scope()),
        Err(ReplacementError::Limit(_))
    ));
}

#[test]
fn malformed_utf8_blank_records_partial_lines_and_deep_nesting_fail_without_repair() {
    let fixture = ledger();
    for bytes in [
        Vec::new(),
        b"\n".to_vec(),
        b" \n".to_vec(),
        vec![0xff, b'\n'],
        b"{}".to_vec(),
        b"{}\n\n".to_vec(),
    ] {
        assert!(inspect_ledger(&bytes, fixture.scope()).is_err());
    }
    let nested = format!("{}0{}\n", "[".repeat(150), "]".repeat(150));
    assert!(inspect_ledger(nested.as_bytes(), fixture.scope()).is_err());
    let oversized = vec![b' '; MAX_LEDGER + 1];
    assert!(matches!(
        inspect_ledger(&oversized, fixture.scope()),
        Err(ReplacementError::Limit(_))
    ));
}

#[test]
fn native_encoding_policy_preserves_local_units_and_rejects_foreign_encoding() {
    let mut fixture = ledger();
    fixture.start();
    #[cfg(windows)]
    let (local, foreign) = (
        NativePath::WindowsUtf16(vec![0xd800]),
        NativePath::UnixBytes(b"file".to_vec()),
    );
    #[cfg(unix)]
    let (local, foreign) = (
        NativePath::UnixBytes(vec![0xff]),
        NativePath::WindowsUtf16(vec![102]),
    );
    fixture.events[1]["action"]["payload"]["target"] =
        serde_json::to_value(&local).expect("local raw path");
    assert_eq!(
        inspect_ledger(&fixture.bytes(), fixture.scope())
            .expect("lossless local target")
            .operations[0]
            .target,
        local
    );
    fixture.events[1]["action"]["payload"]["target"] =
        serde_json::to_value(foreign).expect("foreign path");
    assert!(inspect_ledger(&fixture.bytes(), fixture.scope()).is_err());
}

#[test]
fn exact_ledger_byte_cap_is_accepted_and_one_more_byte_is_refused() {
    let mut fixture = ledger();
    for _ in 0..64 {
        fixture.operation = Uuid::new_v4();
        fixture.start();
        fixture.finish("failed");
    }
    let raw = fixture.bytes();
    let mut remaining = MAX_LEDGER - raw.len();
    let mut padded = Vec::with_capacity(MAX_LEDGER);
    for line in raw.split_inclusive(|byte| *byte == b'\n') {
        let padding = remaining.min(MAX_RECORD - line.len());
        padded.extend_from_slice(&line[..line.len() - 1]);
        padded.resize(padded.len() + padding, b' ');
        padded.push(b'\n');
        remaining -= padding;
    }
    assert_eq!(remaining, 0);
    assert_eq!(padded.len(), MAX_LEDGER);
    assert_eq!(
        inspect_ledger(&padded, fixture.scope())
            .expect("exact total bound")
            .operations
            .len(),
        64
    );
    padded.insert(padded.len() - 1, b' ');
    assert!(matches!(
        inspect_ledger(&padded, fixture.scope()),
        Err(ReplacementError::Limit(_))
    ));
}

#[test]
fn size_descriptors_accept_their_exact_caps_and_empty_file_payloads() {
    let mut fixture = ledger();
    fixture.start();
    fixture.events[1]["action"]["payload"]["expected"]["bytes"] = json!(1_048_576);
    fixture.events[1]["action"]["payload"]["proposed"]["bytes"] = json!(0);
    fixture.prepared();
    fixture.events[2]["action"]["payload"]["manifest"]["bytes"] = json!(65_536);
    assert!(inspect_ledger(&fixture.bytes(), fixture.scope()).is_ok());
}

#[test]
fn acknowledgement_and_uncertainty_retain_each_interrupted_phase() {
    for step in 0..4 {
        for uncertain in [false, true] {
            let mut fixture = ledger();
            fixture.start();
            if step >= 1 {
                fixture.prepared();
            }
            if step >= 2 {
                fixture.staged();
            }
            if step >= 3 {
                fixture.commit();
            }
            let prior = inspect_ledger(&fixture.bytes(), fixture.scope())
                .expect("prefix")
                .operations[0]
                .progress;
            if uncertain {
                fixture.finish("uncertain");
            }
            fixture.acknowledge();
            let snapshot =
                inspect_ledger(&fixture.bytes(), fixture.scope()).expect("acknowledged evidence");
            assert!(snapshot.pending().is_none());
            assert_eq!(snapshot.operations[0].progress, prior);
            assert_eq!(
                snapshot.operations[0].outcome,
                uncertain.then_some(ReplacementOutcome::Uncertain)
            );
            assert!(snapshot.operations[0].acknowledged);
        }
    }
}

#[test]
fn terminal_records_cannot_be_repeated_or_advanced_after_uncertainty() {
    let mut completed = ledger();
    completed.start();
    completed.prepared();
    completed.staged();
    completed.commit();
    completed.finish("succeeded");
    completed.acknowledge();
    assert!(inspect_ledger(&completed.bytes(), completed.scope()).is_err());
    let mut duplicate = ledger();
    duplicate.start();
    duplicate.finish("failed");
    duplicate.finish("failed");
    assert!(inspect_ledger(&duplicate.bytes(), duplicate.scope()).is_err());
    for phase in ["prepare", "stage", "commit", "finish"] {
        let mut fixture = ledger();
        fixture.start();
        fixture.finish("uncertain");
        match phase {
            "prepare" => fixture.prepared(),
            "stage" => fixture.staged(),
            "commit" => fixture.commit(),
            "finish" => fixture.finish("failed"),
            _ => unreachable!("fixed test cases"),
        }
        assert!(
            inspect_ledger(&fixture.bytes(), fixture.scope()).is_err(),
            "{phase}"
        );
    }
}

#[test]
fn initialization_rejects_duplicate_unknown_and_invalid_envelope_data() {
    let mut fixture = Fixture::new();
    fixture.push("initializing", None);
    fixture.push("ready", None);
    let original = String::from_utf8(fixture.bytes()).expect("anchor JSON");
    let duplicate = original.replacen(
        "\"schema_version\":1",
        "\"schema_version\":1,\"schema_version\":1",
        1,
    );
    assert!(inspect_initialization(duplicate.as_bytes(), fixture.control).is_err());
    for (index, key, value) in [
        (0, "extra", json!(true)),
        (1, "sequence", json!(0)),
        (0, "schema_version", json!(0)),
        (0, "replacement_store_id", json!(Uuid::nil())),
        (0, "timestamp", json!("2026-11-09T16:00:00+01:00")),
        (1, "event_id", fixture.events[0]["event_id"].clone()),
    ] {
        let mut events = fixture.events.clone();
        events[index][key] = value;
        assert!(
            inspect_initialization(&encode(&events), fixture.control).is_err(),
            "{key}"
        );
    }
}

#[test]
fn unsupported_variants_invalid_identity_shapes_and_non_v4_ids_are_rejected() {
    let mut fixture = ledger();
    fixture.start();
    fixture.prepared();
    fixture.staged();
    let mut unknown = fixture.events.clone();
    unknown[1]["action"]["type"] = json!("execute_host_command");
    assert!(inspect_ledger(&encode(&unknown), fixture.scope()).is_err());
    for identity in [
        json!({"volume": 7, "file_id": vec![0;15]}),
        json!({"volume": 7, "file_id": vec![0;17]}),
        json!({"volume": 7, "file_id": vec![0;16], "extra": true}),
    ] {
        let mut events = fixture.events.clone();
        events[3]["action"]["payload"]["identity"] = identity;
        assert!(inspect_ledger(&encode(&events), fixture.scope()).is_err());
    }
    let mut bytes = *Uuid::new_v4().as_bytes();
    bytes[8] = 0;
    let non_rfc = Uuid::from_bytes(bytes);
    let non_v4 =
        Uuid::parse_str("6ba7b810-9dad-11d1-80b4-00c04fd430c8").expect("version-1 test UUID");
    for invalid in [non_rfc, non_v4] {
        assert!(ReplacementScope::new(invalid, fixture.store).is_err());
        let mut events = fixture.events.clone();
        events[1]["action"]["payload"]["operation_id"] = json!(invalid);
        assert!(inspect_ledger(&encode(&events), fixture.scope()).is_err());
    }
}

#[test]
fn machine_codes_are_required_for_non_success_and_display_does_not_echo_untrusted_json() {
    let mut fixture = ledger();
    fixture.start();
    fixture.finish("failed");
    fixture.events[2]["action"]["payload"]
        .as_object_mut()
        .expect("payload")
        .remove("code");
    assert!(inspect_ledger(&fixture.bytes(), fixture.scope()).is_err());
    let mut success = ledger();
    success.start();
    success.prepared();
    success.staged();
    success.commit();
    success.finish("succeeded");
    success.events[5]["action"]["payload"]["code"] = json!("unexpected_code");
    assert!(inspect_ledger(&success.bytes(), success.scope()).is_err());
    let mut hostile = ledger();
    hostile.events[0]["action"]["type"] = json!("secret_value_do_not_echo");
    let error = inspect_ledger(&hostile.bytes(), hostile.scope()).expect_err("unsupported action");
    assert!(!error.to_string().contains("secret_value_do_not_echo"));
}

#[test]
fn unit_actions_reject_object_payloads_and_stage_identity_rejects_duplicate_keys() {
    let mut header = ledger();
    header.events[0]["action"]["payload"] = json!({"untrusted": true});
    assert!(inspect_ledger(&header.bytes(), header.scope()).is_err());
    let mut anchor = Fixture::new();
    anchor.push("initializing", None);
    anchor.push("ready", None);
    for index in [0, 1] {
        let mut events = anchor.events.clone();
        events[index]["action"]["payload"] = json!({"untrusted": true});
        assert!(inspect_initialization(&encode(&events), anchor.control).is_err());
    }
    let mut fixture = ledger();
    fixture.start();
    fixture.prepared();
    fixture.staged();
    let original = String::from_utf8(fixture.bytes()).expect("stage JSON");
    let duplicate = original.replacen("\"volume\":7", "\"volume\":7,\"volume\":7", 1);
    assert!(inspect_ledger(duplicate.as_bytes(), fixture.scope()).is_err());
}
