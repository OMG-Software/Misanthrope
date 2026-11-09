//! Strict bounded decoding and historical state transitions; no filesystem I/O.

use std::collections::HashSet;

use orangecode_platform::broker::RelativePath;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};
use uuid::{Uuid, Variant, Version};

use super::{
    InitializationPhase, InitializationSnapshot, RecordedArtifact, RecordedFileIdentity,
    RecordedReplacement, ReplacementError, ReplacementOutcome, ReplacementProgress,
    ReplacementScope, ReplacementSnapshot,
};
use crate::policy::WorkspaceId;
use crate::session::NativePath;
use crate::storage::parse_strict_json;

const MAX_LEDGER_BYTES: usize = 8 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 64 * 1024;
const MAX_LEDGER_RECORDS: usize = 4096;
const MAX_OPERATIONS: usize = 64;
const MAX_PAYLOAD_BYTES: u64 = 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<A> {
    schema_version: u32,
    control_store_id: Uuid,
    replacement_store_id: Uuid,
    event_id: Uuid,
    sequence: u64,
    timestamp: String,
    action: A,
}

#[derive(Deserialize)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum InitializationAction {
    Initializing,
    Ready,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactNames {
    original: String,
    proposed: String,
    manifest: String,
    stage: String,
}

#[derive(Deserialize)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum LedgerAction {
    Created,
    ReplacementStarted {
        operation_id: Uuid,
        session_id: Uuid,
        workspace_id: Uuid,
        revision: u64,
        target: NativePath,
        expected: RecordedArtifact,
        proposed: RecordedArtifact,
        artifacts: ArtifactNames,
    },
    CheckpointPrepared {
        operation_id: Uuid,
        manifest: RecordedArtifact,
    },
    StagePrepared {
        operation_id: Uuid,
        identity: RecordedFileIdentity,
        content: RecordedArtifact,
    },
    CommitIntended {
        operation_id: Uuid,
    },
    ReplacementFinished {
        operation_id: Uuid,
        outcome: ReplacementOutcome,
        code: Option<String>,
    },
    ReplacementAcknowledged {
        operation_id: Uuid,
    },
}

/// Check framing and all byte/count bounds before parsing or allocating JSON.
fn records(
    bytes: &[u8],
    max_bytes: usize,
    max_records: usize,
    max_record_bytes: usize,
) -> Result<impl Iterator<Item = &[u8]>, ReplacementError> {
    if bytes.len() > max_bytes {
        return Err(ReplacementError::Limit("total bytes"));
    }
    if bytes.is_empty() || !bytes.ends_with(b"\n") {
        return Err(ReplacementError::Invalid("empty or incomplete JSONL"));
    }
    for (index, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        if index >= max_records {
            return Err(ReplacementError::Limit("record count"));
        }
        if line.len() > max_record_bytes {
            return Err(ReplacementError::Limit("record bytes"));
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            return Err(ReplacementError::Invalid("blank JSONL record"));
        }
    }
    Ok(bytes.split_inclusive(|byte| *byte == b'\n'))
}

fn decode<T: DeserializeOwned>(line: &[u8]) -> Result<T, ReplacementError> {
    let value = parse_strict_json(line).map_err(ReplacementError::Json)?;
    serde_json::from_value(value).map_err(ReplacementError::Json)
}

pub(super) fn validate_uuid(value: Uuid) -> Result<(), ReplacementError> {
    if value.is_nil()
        || value.get_version() != Some(Version::Random)
        || value.get_variant() != Variant::RFC4122
    {
        return Err(ReplacementError::Invalid(
            "ID is not an RFC 4122 version-4 UUID",
        ));
    }
    Ok(())
}

fn validate_envelope<A>(
    entry: &Envelope<A>,
    index: usize,
    scope: ReplacementScope,
) -> Result<(), ReplacementError> {
    validate_uuid(entry.event_id)?;
    if entry.schema_version != 1
        || entry.control_store_id != scope.control_store_id
        || entry.replacement_store_id != scope.replacement_store_id
        || entry.sequence
            != u64::try_from(index).map_err(|_| ReplacementError::Limit("sequence"))?
    {
        return Err(ReplacementError::Invalid(
            "schema, scope, or sequence mismatch",
        ));
    }
    if entry.timestamp.len() > 64 {
        return Err(ReplacementError::Invalid("timestamp is too long"));
    }
    let timestamp = OffsetDateTime::parse(&entry.timestamp, &Rfc3339)
        .map_err(|_| ReplacementError::Invalid("invalid timestamp"))?;
    if timestamp.offset() != UtcOffset::UTC {
        return Err(ReplacementError::Invalid("timestamp is not UTC"));
    }
    Ok(())
}

pub(super) fn initialization(
    bytes: &[u8],
    expected_control_store_id: Uuid,
) -> Result<InitializationSnapshot, ReplacementError> {
    validate_uuid(expected_control_store_id)?;
    let mut snapshot: Option<InitializationSnapshot> = None;
    let mut first_id = None;
    for (index, line) in records(bytes, 4096, 2, 4096)?.enumerate() {
        let entry: Envelope<InitializationAction> = decode(line)?;
        let scope = ReplacementScope::new(expected_control_store_id, entry.replacement_store_id)?;
        validate_envelope(&entry, index, scope)?;
        if let Some(previous) = &snapshot
            && (previous.scope != scope || first_id == Some(entry.event_id))
        {
            return Err(ReplacementError::Invalid(
                "initialization identity changed or repeated",
            ));
        }
        let phase = match (index, entry.action) {
            (0, InitializationAction::Initializing) => InitializationPhase::Initializing,
            (1, InitializationAction::Ready) => InitializationPhase::Ready,
            _ => {
                return Err(ReplacementError::Invalid(
                    "invalid initialization transition",
                ));
            }
        };
        if index == 0 {
            first_id = Some(entry.event_id);
        }
        snapshot = Some(InitializationSnapshot {
            scope,
            phase,
            event_count: index + 1,
            encoded_bytes: bytes.len(),
        });
    }
    snapshot.ok_or(ReplacementError::Invalid("missing initialization record"))
}

pub(super) fn ledger(
    bytes: &[u8],
    scope: ReplacementScope,
) -> Result<ReplacementSnapshot, ReplacementError> {
    let mut snapshot = ReplacementSnapshot {
        scope,
        operations: Vec::new(),
        event_count: 0,
        encoded_bytes: bytes.len(),
        pending_operation_id: None,
    };
    let mut event_ids = HashSet::new();
    for (index, line) in records(
        bytes,
        MAX_LEDGER_BYTES,
        MAX_LEDGER_RECORDS,
        MAX_RECORD_BYTES,
    )?
    .enumerate()
    {
        let entry: Envelope<LedgerAction> = decode(line)?;
        validate_envelope(&entry, index, scope)?;
        if !event_ids.insert(entry.event_id) {
            return Err(ReplacementError::Invalid("duplicate event ID"));
        }
        if (index == 0) != matches!(entry.action, LedgerAction::Created) {
            return Err(ReplacementError::Invalid(
                "created record must occur exactly first",
            ));
        }
        apply(&mut snapshot, entry.action)?;
        snapshot.event_count = index + 1;
    }
    Ok(snapshot)
}

fn validate_artifact(
    artifact: &RecordedArtifact,
    max_bytes: u64,
    allow_empty: bool,
) -> Result<(), ReplacementError> {
    if artifact.bytes > max_bytes || (!allow_empty && artifact.bytes == 0) {
        return Err(ReplacementError::Invalid(
            "artifact size is outside its bound",
        ));
    }
    if artifact.sha256.len() != 64
        || !artifact
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ReplacementError::Invalid(
            "artifact digest is not lowercase SHA-256",
        ));
    }
    Ok(())
}

fn validate_target(target: &NativePath) -> Result<(), ReplacementError> {
    let path = target
        .to_path_buf()
        .map_err(|_| ReplacementError::Invalid("target encoding does not match this OS"))?;
    let relative = RelativePath::new(&path)
        .map_err(|_| ReplacementError::Invalid("invalid workspace-relative target"))?;
    if relative.is_empty() {
        return Err(ReplacementError::Invalid(
            "target cannot be the workspace root",
        ));
    }
    Ok(())
}

fn pending(
    snapshot: &mut ReplacementSnapshot,
    id: Uuid,
) -> Result<&mut RecordedReplacement, ReplacementError> {
    if snapshot.pending_operation_id != Some(id) {
        return Err(ReplacementError::Invalid(
            "event does not match the unresolved operation",
        ));
    }
    snapshot
        .operations
        .last_mut()
        .filter(|operation| operation.operation_id == id)
        .ok_or(ReplacementError::Invalid("missing unresolved operation"))
}

fn apply(snapshot: &mut ReplacementSnapshot, action: LedgerAction) -> Result<(), ReplacementError> {
    match action {
        LedgerAction::Created => {}
        LedgerAction::ReplacementStarted {
            operation_id,
            session_id,
            workspace_id,
            revision,
            target,
            expected,
            proposed,
            artifacts,
        } => {
            validate_uuid(operation_id)?;
            validate_uuid(session_id)?;
            validate_uuid(workspace_id)?;
            if snapshot.pending_operation_id.is_some()
                || snapshot
                    .operations
                    .iter()
                    .any(|operation| operation.operation_id == operation_id)
            {
                return Err(ReplacementError::Invalid("parallel or reused operation ID"));
            }
            if snapshot.operations.len() >= MAX_OPERATIONS {
                return Err(ReplacementError::Limit("retained operations"));
            }
            if snapshot
                .operations
                .last()
                .is_some_and(|operation| revision < operation.revision)
            {
                return Err(ReplacementError::Invalid("policy revision moved backwards"));
            }
            validate_target(&target)?;
            validate_artifact(&expected, MAX_PAYLOAD_BYTES, true)?;
            validate_artifact(&proposed, MAX_PAYLOAD_BYTES, true)?;
            if artifacts.original != "original"
                || artifacts.proposed != "proposed"
                || artifacts.manifest != "manifest.json"
                || artifacts.stage != "stage"
            {
                return Err(ReplacementError::Invalid(
                    "artifact names do not match the fixed private layout",
                ));
            }
            let workspace_id = WorkspaceId::from_uuid(workspace_id)
                .map_err(|_| ReplacementError::Invalid("invalid workspace ID"))?;
            snapshot.operations.push(RecordedReplacement {
                operation_id,
                session_id,
                workspace_id,
                revision,
                target,
                expected,
                proposed,
                manifest: None,
                stage_identity: None,
                progress: ReplacementProgress::Intent,
                outcome: None,
                code: None,
                acknowledged: false,
            });
            snapshot.pending_operation_id = Some(operation_id);
        }
        LedgerAction::CheckpointPrepared {
            operation_id,
            manifest,
        } => {
            let operation = pending(snapshot, operation_id)?;
            if operation.progress != ReplacementProgress::Intent || operation.outcome.is_some() {
                return Err(ReplacementError::Invalid(
                    "checkpoint preparation is out of order",
                ));
            }
            validate_artifact(&manifest, MAX_MANIFEST_BYTES, false)?;
            operation.manifest = Some(manifest);
            operation.progress = ReplacementProgress::CheckpointPrepared;
        }
        LedgerAction::StagePrepared {
            operation_id,
            identity,
            content,
        } => {
            let operation = pending(snapshot, operation_id)?;
            if operation.progress != ReplacementProgress::CheckpointPrepared
                || operation.outcome.is_some()
                || content != operation.proposed
            {
                return Err(ReplacementError::Invalid(
                    "stage is out of order or has different proposed content",
                ));
            }
            operation.stage_identity = Some(identity);
            operation.progress = ReplacementProgress::StagePrepared;
        }
        LedgerAction::CommitIntended { operation_id } => {
            let operation = pending(snapshot, operation_id)?;
            if operation.progress != ReplacementProgress::StagePrepared
                || operation.outcome.is_some()
            {
                return Err(ReplacementError::Invalid("commit intent is out of order"));
            }
            operation.progress = ReplacementProgress::CommitIntended;
        }
        LedgerAction::ReplacementFinished {
            operation_id,
            outcome,
            code,
        } => {
            let operation = pending(snapshot, operation_id)?;
            if operation.outcome.is_some() {
                return Err(ReplacementError::Invalid("duplicate terminal outcome"));
            }
            let committed = operation.progress == ReplacementProgress::CommitIntended;
            if (outcome == ReplacementOutcome::Succeeded && !committed)
                || (committed
                    && matches!(
                        outcome,
                        ReplacementOutcome::Failed | ReplacementOutcome::Conflict
                    ))
            {
                return Err(ReplacementError::Invalid(
                    "outcome is incompatible with commit state",
                ));
            }
            let valid_code = code.as_ref().is_some_and(|value| {
                !value.is_empty()
                    && value.len() <= 128
                    && value.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            });
            if (outcome == ReplacementOutcome::Succeeded && code.is_some())
                || (outcome != ReplacementOutcome::Succeeded && !valid_code)
            {
                return Err(ReplacementError::Invalid("invalid terminal machine code"));
            }
            operation.outcome = Some(outcome);
            operation.code = code;
            if outcome != ReplacementOutcome::Uncertain {
                snapshot.pending_operation_id = None;
            }
        }
        LedgerAction::ReplacementAcknowledged { operation_id } => {
            let operation = pending(snapshot, operation_id)?;
            operation.acknowledged = true;
            snapshot.pending_operation_id = None;
        }
    }
    Ok(())
}
