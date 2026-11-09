//! Bounded, single-writer JSONL persistence for a OrangeCode session.
//!
//! Journals are untrusted input when opened. This module validates the complete
//! file before returning a writable handle. It does not execute or resume work.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};
use uuid::Uuid;

use crate::storage::{self, StorageError};

const SCHEMA_VERSION_V1: u32 = 1;
const SCHEMA_VERSION: u32 = 2;
const MAX_TITLE_BYTES: usize = 256;
pub(crate) const MAX_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_EVENT_BYTES: usize = 64 * 1024;
const MAX_JOURNAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_EVENTS: usize = 10_000;
const MAX_FAILURE_CODE_BYTES: usize = 128;
const MAX_TOOL_CALL_ID_BYTES: usize = 128;
const MAX_TOOL_NAME_BYTES: usize = 64;
const MAX_TOOL_ARGUMENTS_BYTES: usize = 4 * 1024;
const MAX_TOOL_ERROR_CODE_BYTES: usize = 128;
const MAX_TOOL_OUTPUT_DIGEST_BYTES: usize = 64;
const MAX_TOOL_NAMES: usize = 8;
const MAX_TOOL_SCHEMA_VERSION_BYTES: usize = 32;
// A terminal envelope plus the most-escaped allowed failure code fits in 1 KiB.
const TERMINAL_RESERVE_BYTES: usize = 1024;

/// A failure while creating, reading, or changing a session journal.
#[derive(Debug, Error)]
pub enum SessionError {
    /// An operating-system file operation failed.
    #[error("session journal I/O failed: {0}")]
    Io(#[from] io::Error),
    /// The journal is malformed, unsupported, or inconsistent.
    #[error("invalid session journal: {0}")]
    InvalidJournal(&'static str),
    /// A requested value exceeds a documented limit or is invalid.
    #[error("invalid session value: {0}")]
    InvalidValue(&'static str),
    /// The journal has reached its event or byte limit.
    #[error("session journal limit reached")]
    LimitReached,
    /// A different process or handle already owns the writer lock.
    #[error("session journal is already open for writing")]
    Busy,
    /// The journal has an active turn and cannot be archived.
    #[error("an active turn prevents archiving")]
    ActiveTurn,
    /// The requested turn is not the active turn.
    #[error("request does not identify the active turn")]
    WrongTurn,
    /// This journal cannot accept further writes after an uncertain write error.
    #[error("session journal writer is poisoned after a write or sync failure")]
    Poisoned,
}

/// The only roles accepted by this API. A transcript role never grants authority.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    /// A message entered by the user.
    User,
    /// A message returned by the assistant.
    Assistant,
    /// A core-observed tool result. Schema version 2 only.
    Tool,
}

/// Lossless native path metadata. It is descriptive data, not an access grant.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "encoding",
    content = "units",
    deny_unknown_fields,
    rename_all = "snake_case"
)]
pub enum NativePath {
    /// Unix path bytes, which may not be valid UTF-8.
    UnixBytes(Vec<u8>),
    /// Windows UTF-16 code units, which may include unpaired surrogates.
    WindowsUtf16(Vec<u16>),
}

impl NativePath {
    /// Encode a native path without replacing invalid Unicode. This grants no access.
    pub fn from_path(path: &Path) -> Result<Self, SessionError> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Ok(Self::UnixBytes(path.as_os_str().as_bytes().to_vec()))
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            Ok(Self::WindowsUtf16(path.as_os_str().encode_wide().collect()))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            Err(SessionError::InvalidValue(
                "native paths are unsupported on this platform",
            ))
        }
    }

    /// Restore a path on its originating OS without a lossy text conversion.
    /// Returns an error for a foreign encoding; it never guesses a translation.
    pub fn to_path_buf(&self) -> Result<PathBuf, SessionError> {
        #[cfg(unix)]
        if let Self::UnixBytes(units) = self {
            use std::os::unix::ffi::OsStringExt;
            return Ok(PathBuf::from(std::ffi::OsString::from_vec(units.clone())));
        }
        #[cfg(windows)]
        if let Self::WindowsUtf16(units) = self {
            use std::os::windows::ffi::OsStringExt;
            return Ok(PathBuf::from(std::ffi::OsString::from_wide(units)));
        }
        Err(SessionError::InvalidValue(
            "path encoding does not match this OS",
        ))
    }
}

/// One user or assistant message stored in the transcript snapshot.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedMessage {
    /// The untrusted display role recorded for this message.
    pub role: MessageRole,
    /// Message text, bounded to 16 KiB in UTF-8.
    pub text: String,
}

/// The validated state reconstructed from a journal.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSnapshot {
    /// The journal schema version that every event must share.
    pub schema_version: u32,
    /// The UUID shared by all events in this journal.
    pub session_id: Uuid,
    /// Stable identifier for the workspace association recorded at creation.
    pub workspace_id: Uuid,
    /// Whether the session is archived.
    pub archived: bool,
    /// The currently active turn, if any. Opening never resumes it.
    pub active_turn: Option<Uuid>,
    /// User, assistant, and schema-v2 tool messages in append order.
    pub messages: Vec<RecordedMessage>,
    /// Current session title.
    pub title: String,
    /// Canonical workspace path encoded as native units.
    pub workspace: NativePath,
    /// Number of validated events, including the initial event.
    pub event_count: u64,
}

/// Turn-local tool-call bookkeeping used to reject malformed append sequences.
#[derive(Debug, Default)]
struct ToolState {
    pending: HashSet<String>,
    completed: HashSet<String>,
    messaged: HashSet<String>,
    outputs: HashMap<String, (u64, String)>,
}

impl ToolState {
    /// Forget all call identifiers when a turn ends or a new one starts.
    fn clear(&mut self) {
        self.pending.clear();
        self.completed.clear();
        self.messaged.clear();
        self.outputs.clear();
    }
}

/// An exclusively locked session journal.
///
/// The lock remains held for this value's lifetime. A failed append or sync
/// poisons the writer because the durable file may contain an uncertain tail.
#[derive(Debug)]
pub struct SessionJournal {
    path: PathBuf,
    file: File,
    snapshot: SessionSnapshot,
    schema_version: u32,
    tool_state: ToolState,
    bytes: u64,
    next_seq: u64,
    poisoned: bool,
    #[cfg(test)]
    fail_next_sync: bool,
}

impl SessionJournal {
    /// Create a new journal below `data_root` and record its initial metadata.
    ///
    /// `data_root` must come from trusted application settings. `workspace` is
    /// canonicalized and recorded only as metadata; this function grants no file
    /// access. A fresh per-session workspace ID is generated for legacy callers.
    /// The session event is synced before this function returns.
    pub fn create(data_root: &Path, workspace: &Path, title: &str) -> Result<Self, SessionError> {
        Self::create_with_id(data_root, workspace, title, Uuid::new_v4())
    }

    /// Create a journal with a caller-validated workspace ID in its metadata.
    ///
    /// `workspace_id` is descriptive metadata only. Never derive execution
    /// authority from this journal header. The caller must obtain the ID from
    /// current trusted control state. `data_root` must come from trusted
    /// application settings. `workspace` is canonicalized and recorded only as
    /// metadata; this function grants no file access.
    pub fn create_with_workspace_id(
        data_root: &Path,
        workspace: &Path,
        title: &str,
        workspace_id: crate::policy::WorkspaceId,
    ) -> Result<Self, SessionError> {
        Self::create_with_id(data_root, workspace, title, *workspace_id.as_uuid())
    }

    /// Create a journal after the public constructors select its metadata ID.
    fn create_with_id(
        data_root: &Path,
        workspace: &Path,
        title: &str,
        workspace_id: Uuid,
    ) -> Result<Self, SessionError> {
        validate_title(title)?;
        let workspace = fs::canonicalize(workspace)?;
        if !workspace.is_dir() {
            return Err(SessionError::InvalidValue(
                "workspace must resolve to a directory",
            ));
        }
        let workspace = NativePath::from_path(&workspace)?;
        let now = OffsetDateTime::now_utc();
        let session_id = Uuid::new_v4();
        let dir = data_root
            .join("sessions")
            .join(format!("{:04}", now.year()))
            .join(format!("{:02}", u8::from(now.month())))
            .join(format!("{:02}", now.day()));
        storage::create_private_directories(&dir).map_err(map_storage_error)?;
        let filename = format!(
            "orangecode-{:02}-{:02}-{:02}-{:03}Z-{session_id}.jsonl",
            now.hour(),
            now.minute(),
            now.second(),
            now.millisecond()
        );
        let path = dir.join(filename);
        let mut file = storage::open_locked_file(&path, true).map_err(map_storage_error)?;

        let started = SessionStarted {
            title: title.to_owned(),
            workspace,
            workspace_id,
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        let event = EventRecord {
            schema_version: SCHEMA_VERSION,
            session_id,
            event_id: Uuid::new_v4(),
            seq: 0,
            timestamp: format_timestamp(now)?,
            event_type: "session.started",
            payload: &started,
        };
        let bytes = encode_event(&event)?;
        if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
            return Err(SessionError::Io(error));
        }
        storage::sync_parent_directory(&dir).map_err(map_storage_error)?;

        Ok(Self {
            path,
            file,
            snapshot: SessionSnapshot {
                schema_version: SCHEMA_VERSION,
                session_id,
                workspace_id: started.workspace_id,
                archived: false,
                active_turn: None,
                messages: Vec::new(),
                title: title.to_owned(),
                workspace: started.workspace,
                event_count: 1,
            },
            schema_version: SCHEMA_VERSION,
            tool_state: ToolState::default(),
            bytes: u64::try_from(bytes.len()).map_err(|_| SessionError::LimitReached)?,
            next_seq: 1,
            poisoned: false,
            #[cfg(test)]
            fail_next_sync: false,
        })
    }

    /// Open and fully validate an existing journal for exclusive writing.
    ///
    /// Unknown schemas, malformed events, duplicate keys or IDs, incomplete
    /// tails, and unsupported state transitions are rejected without repair.
    /// The journal is never replayed as executable work.
    pub fn open(path: &Path) -> Result<Self, SessionError> {
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(SessionError::InvalidJournal("filename is not valid UTF-8"))?;
        let id_text = filename
            .strip_prefix("orangecode-")
            .and_then(|name| name.strip_suffix(".jsonl"))
            .and_then(|name| name.rsplit_once("Z-"))
            .map(|(_, id)| id)
            .ok_or(SessionError::InvalidJournal(
                "filename does not contain a session UUID",
            ))?;
        let filename_id = Uuid::parse_str(id_text)
            .map_err(|_| SessionError::InvalidJournal("filename has an invalid session UUID"))?;
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SessionError::InvalidJournal(
                "journal is not a regular file",
            ));
        }
        let mut file = storage::open_locked_file(path, false).map_err(map_storage_error)?;
        let contents = read_bounded(&mut file)?;
        let (snapshot, tool_state) = replay(&contents, filename_id)?;
        let bytes = u64::try_from(contents.len()).map_err(|_| SessionError::LimitReached)?;
        let next_seq = snapshot.event_count;
        let schema_version = snapshot.schema_version;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            snapshot,
            schema_version,
            tool_state,
            bytes,
            next_seq,
            poisoned: false,
            #[cfg(test)]
            fail_next_sync: false,
        })
    }

    /// Return the stable path of this journal.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Return the journal schema version validated on open.
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Return the last fully validated durable snapshot.
    pub fn snapshot(&self) -> &SessionSnapshot {
        &self.snapshot
    }

    /// Check a complete CLI request before recording its turn marker.
    ///
    /// This checks encoded event sizes and reserves one maximum-size assistant
    /// event plus a terminal record. No journal bytes or snapshot state change.
    /// The exclusive writer must retain ownership until dispatch and completion.
    pub fn preflight_request(
        &self,
        text: &str,
        manifest: &RequestManifest,
    ) -> Result<(), SessionError> {
        self.ensure_mutable()?;
        if self.snapshot.archived || self.snapshot.active_turn.is_some() {
            return Err(SessionError::InvalidValue(
                "request requires an idle unarchived session",
            ));
        }
        if text.len() > MAX_MESSAGE_BYTES {
            return Err(SessionError::InvalidValue("message exceeds 16 KiB"));
        }
        validate_manifest(manifest, self.schema_version)?;
        let request_id = Uuid::nil();
        let mut seq = self.next_seq;
        let mut size = self.bytes;
        // A nine-digit fractional timestamp bounds any timestamp written later.
        let timestamp = "2026-11-08T00:00:00.999999999Z";
        let mut measure = |kind, payload: Value| -> Result<(), SessionError> {
            let encoded = encode_event(&EventRecord {
                schema_version: self.schema_version,
                session_id: self.snapshot.session_id,
                event_id: Uuid::nil(),
                seq,
                timestamp: timestamp.to_owned(),
                event_type: kind,
                payload: &payload,
            })?;
            size = size
                .checked_add(u64::try_from(encoded.len()).map_err(|_| SessionError::LimitReached)?)
                .ok_or(SessionError::LimitReached)?;
            seq = seq.checked_add(1).ok_or(SessionError::LimitReached)?;
            Ok(())
        };
        measure("turn.started", serde_json::json!({"request_id":request_id}))?;
        measure(
            "message.recorded",
            serde_json::json!({"role":"user","source":"user_cli","text":text}),
        )?;
        measure(
            "request.manifest",
            serde_json::json!({"request_id":request_id,"manifest":manifest}),
        )?;
        let final_bytes = size
            .checked_add(
                u64::try_from(MAX_EVENT_BYTES + TERMINAL_RESERVE_BYTES)
                    .map_err(|_| SessionError::LimitReached)?,
            )
            .ok_or(SessionError::LimitReached)?;
        let final_count = seq.checked_add(2).ok_or(SessionError::LimitReached)?;
        if final_bytes > u64::try_from(MAX_JOURNAL_BYTES).map_err(|_| SessionError::LimitReached)?
            || final_count > u64::try_from(MAX_EVENTS).map_err(|_| SessionError::LimitReached)?
        {
            return Err(SessionError::LimitReached);
        }
        Ok(())
    }

    /// Append a bounded user or assistant message and sync it to disk.
    pub fn record_message(&mut self, role: MessageRole, text: &str) -> Result<(), SessionError> {
        if text.len() > MAX_MESSAGE_BYTES {
            return Err(SessionError::InvalidValue("message exceeds 16 KiB"));
        }
        if self.snapshot.archived {
            return Err(SessionError::InvalidValue(
                "archived sessions cannot be changed",
            ));
        }
        let source = match role {
            MessageRole::User => MessageSource::UserCli,
            MessageRole::Assistant => MessageSource::Provider,
            MessageRole::Tool => {
                return Err(SessionError::InvalidValue(
                    "use record_tool_message for tool messages",
                ));
            }
        };
        let payload = MessagePayload {
            role,
            source,
            text: text.to_owned(),
            call_id: None,
        };
        let bytes = self.append("message.recorded", &payload)?;
        self.snapshot.messages.push(RecordedMessage {
            role,
            text: text.to_owned(),
        });
        self.commit_accounting(bytes)?;
        Ok(())
    }

    /// Record a model-requested tool call before dispatch. Schema version 2 only.
    ///
    /// The call identifier, tool name, and arguments are untrusted model data.
    /// Recording an intent never grants permission and replay never dispatches it.
    pub fn record_tool_request(
        &mut self,
        request_id: Uuid,
        call_id: &str,
        tool: &str,
        arguments: &str,
    ) -> Result<(), SessionError> {
        self.ensure_mutable()?;
        self.require_schema_v2()?;
        if self.snapshot.active_turn != Some(request_id) {
            return Err(SessionError::WrongTurn);
        }
        validate_tool_call_id(call_id)?;
        validate_tool_name(tool)?;
        validate_tool_arguments(arguments)?;
        if self.tool_state.pending.contains(call_id) || self.tool_state.completed.contains(call_id)
        {
            return Err(SessionError::InvalidValue("duplicate tool call identifier"));
        }
        let payload = ToolRequestedPayload {
            request_id,
            call_id: call_id.to_owned(),
            tool: tool.to_owned(),
            arguments: arguments.to_owned(),
        };
        let bytes = self.append("tool.requested", &payload)?;
        self.tool_state.pending.insert(call_id.to_owned());
        self.commit_accounting(bytes)
    }

    /// Record the core-observed outcome of a requested tool call. Schema version 2 only.
    pub fn record_tool_result(
        &mut self,
        request_id: Uuid,
        call_id: &str,
        outcome: ToolOutcome,
        code: Option<&str>,
        output_bytes: u64,
        output_sha256: &str,
    ) -> Result<(), SessionError> {
        self.ensure_mutable()?;
        self.require_schema_v2()?;
        if self.snapshot.active_turn != Some(request_id) {
            return Err(SessionError::WrongTurn);
        }
        validate_tool_call_id(call_id)?;
        if !self.tool_state.pending.contains(call_id) {
            return Err(SessionError::InvalidValue(
                "tool result has no matching pending request",
            ));
        }
        validate_tool_outcome(outcome, code)?;
        validate_tool_output(output_bytes, output_sha256)?;
        let payload = ToolCompletedPayload {
            request_id,
            call_id: call_id.to_owned(),
            outcome,
            code: code.map(str::to_owned),
            output_bytes,
            output_sha256: output_sha256.to_owned(),
        };
        let bytes = self.append("tool.completed", &payload)?;
        self.tool_state.pending.remove(call_id);
        self.tool_state.completed.insert(call_id.to_owned());
        self.tool_state
            .outputs
            .insert(call_id.to_owned(), (output_bytes, output_sha256.to_owned()));
        self.commit_accounting(bytes)
    }

    /// Append the exact source-labelled tool text shown to the model. Schema version 2 only.
    pub fn record_tool_message(&mut self, call_id: &str, text: &str) -> Result<(), SessionError> {
        self.ensure_mutable()?;
        self.require_schema_v2()?;
        if self.snapshot.active_turn.is_none() {
            return Err(SessionError::InvalidValue(
                "tool messages require an active turn",
            ));
        }
        if text.len() > MAX_MESSAGE_BYTES {
            return Err(SessionError::InvalidValue("tool message exceeds 16 KiB"));
        }
        validate_tool_call_id(call_id)?;
        if !self.tool_state.completed.contains(call_id)
            || self.tool_state.messaged.contains(call_id)
        {
            return Err(SessionError::InvalidValue(
                "tool message has no completed, unmessaged call",
            ));
        }
        let expected = self
            .tool_state
            .outputs
            .get(call_id)
            .ok_or(SessionError::InvalidValue(
                "tool message has no recorded output identity",
            ))?;
        let text_bytes = u64::try_from(text.len()).map_err(|_| SessionError::LimitReached)?;
        if expected.0 != text_bytes || expected.1 != sha256_hex(text.as_bytes()) {
            return Err(SessionError::InvalidValue(
                "tool message does not match its recorded output identity",
            ));
        }
        let payload = MessagePayload {
            role: MessageRole::Tool,
            source: MessageSource::CoreTool,
            text: text.to_owned(),
            call_id: Some(call_id.to_owned()),
        };
        let bytes = self.append("message.recorded", &payload)?;
        self.tool_state.outputs.remove(call_id);
        self.tool_state.messaged.insert(call_id.to_owned());
        self.snapshot.messages.push(RecordedMessage {
            role: MessageRole::Tool,
            text: text.to_owned(),
        });
        self.commit_accounting(bytes)
    }

    /// Refuse tool-event operations on a legacy schema-v1 journal.
    fn require_schema_v2(&self) -> Result<(), SessionError> {
        if self.schema_version < SCHEMA_VERSION {
            Err(SessionError::InvalidValue(
                "tool events require session schema version 2",
            ))
        } else {
            Ok(())
        }
    }

    /// Record the exact visible request manifest for the active turn.
    ///
    /// The manifest is descriptive evidence only. It never grants permission
    /// and replay never dispatches its contents.
    pub fn record_manifest(
        &mut self,
        request_id: Uuid,
        manifest: RequestManifest,
    ) -> Result<(), SessionError> {
        self.ensure_mutable()?;
        if self.snapshot.active_turn != Some(request_id) {
            return Err(SessionError::WrongTurn);
        }
        validate_manifest(&manifest, self.schema_version)?;
        let payload = ManifestPayload {
            request_id,
            manifest,
        };
        let bytes = self.append("request.manifest", &payload)?;
        self.commit_accounting(bytes)
    }

    /// Start a turn and return its new request ID. Opening never starts a turn.
    pub fn start_turn(&mut self) -> Result<Uuid, SessionError> {
        self.ensure_mutable()?;
        if self.snapshot.archived {
            return Err(SessionError::InvalidValue(
                "archived sessions cannot start turns",
            ));
        }
        if self.snapshot.active_turn.is_some() {
            return Err(SessionError::ActiveTurn);
        }
        self.tool_state.clear();
        let request_id = Uuid::new_v4();
        let bytes = self.append("turn.started", &TurnPayload { request_id })?;
        self.snapshot.active_turn = Some(request_id);
        self.commit_accounting(bytes)?;
        Ok(request_id)
    }

    /// Complete the active turn identified by `request_id`.
    pub fn complete_turn(&mut self, request_id: Uuid) -> Result<(), SessionError> {
        self.finish_turn(request_id, "turn.completed", None)
    }

    /// Fail the active turn with a bounded, non-secret error code.
    pub fn fail_turn(&mut self, request_id: Uuid, code: &str) -> Result<(), SessionError> {
        if code.is_empty()
            || code.len() > MAX_FAILURE_CODE_BYTES
            || code.chars().any(char::is_control)
        {
            return Err(SessionError::InvalidValue(
                "failure code is invalid or exceeds 128 bytes",
            ));
        }
        self.finish_turn(request_id, "turn.failed", Some(code))
    }

    /// Archive an idle session. Repeated archive calls are idempotent.
    pub fn archive(&mut self) -> Result<(), SessionError> {
        self.ensure_mutable()?;
        if self.snapshot.archived {
            return Ok(());
        }
        if self.snapshot.active_turn.is_some() {
            return Err(SessionError::ActiveTurn);
        }
        let bytes = self.append("session.archived", &EmptyPayload {})?;
        self.snapshot.archived = true;
        self.commit_accounting(bytes)
    }

    /// Restore an archived session without starting or replaying a turn.
    pub fn restore(&mut self) -> Result<(), SessionError> {
        self.ensure_mutable()?;
        if !self.snapshot.archived {
            return Ok(());
        }
        let bytes = self.append("session.restored", &EmptyPayload {})?;
        self.snapshot.archived = false;
        self.commit_accounting(bytes)
    }

    /// Append one terminal event before releasing the active-turn state.
    fn finish_turn(
        &mut self,
        request_id: Uuid,
        event_type: &'static str,
        code: Option<&str>,
    ) -> Result<(), SessionError> {
        self.ensure_mutable()?;
        if self.snapshot.active_turn != Some(request_id) {
            return Err(SessionError::WrongTurn);
        }
        if event_type == "turn.completed"
            && (!self.tool_state.pending.is_empty()
                || self.tool_state.completed != self.tool_state.messaged
                || !self.tool_state.outputs.is_empty())
        {
            return Err(SessionError::InvalidValue(
                "completed turn has unresolved or unrecorded tool calls",
            ));
        }
        let payload = TurnFinishedPayload {
            request_id,
            code: code.map(str::to_owned),
        };
        let bytes = self.append(event_type, &payload)?;
        self.snapshot.active_turn = None;
        self.tool_state.clear();
        self.commit_accounting(bytes)
    }

    /// Refuse all mutations after an uncertain write or sync outcome.
    fn ensure_mutable(&self) -> Result<(), SessionError> {
        if self.poisoned {
            Err(SessionError::Poisoned)
        } else {
            Ok(())
        }
    }

    /// Enforce ordinary quotas while preserving capacity for turn termination.
    /// A legacy full active journal may append exactly one bounded terminal tail.
    fn append<T: Serialize>(
        &mut self,
        event_type: &'static str,
        payload: &T,
    ) -> Result<Vec<u8>, SessionError> {
        self.ensure_mutable()?;
        let now = OffsetDateTime::now_utc();
        let event = EventRecord {
            schema_version: self.schema_version,
            session_id: self.snapshot.session_id,
            event_id: Uuid::new_v4(),
            seq: self.next_seq,
            timestamp: format_timestamp(now)?,
            event_type,
            payload,
        };
        let bytes = encode_event(&event)?;
        let new_len = self
            .bytes
            .checked_add(u64::try_from(bytes.len()).map_err(|_| SessionError::LimitReached)?)
            .ok_or(SessionError::LimitReached)?;
        let terminal = matches!(event_type, "turn.completed" | "turn.failed");
        let event_limit = u64::try_from(MAX_EVENTS).map_err(|_| SessionError::LimitReached)?;
        let byte_limit =
            u64::try_from(MAX_JOURNAL_BYTES).map_err(|_| SessionError::LimitReached)?;
        if terminal {
            if self.snapshot.active_turn.is_none()
                || bytes.len() > TERMINAL_RESERVE_BYTES
                || self.next_seq > event_limit
                || self.bytes > byte_limit
                || new_len
                    > byte_limit
                        + u64::try_from(TERMINAL_RESERVE_BYTES)
                            .map_err(|_| SessionError::LimitReached)?
            {
                return Err(SessionError::LimitReached);
            }
        } else {
            let reserve = self.snapshot.active_turn.is_some() || event_type == "turn.started";
            let required_count = self
                .next_seq
                .checked_add(1 + u64::from(reserve))
                .ok_or(SessionError::LimitReached)?;
            let reserved_bytes = if reserve {
                u64::try_from(TERMINAL_RESERVE_BYTES).map_err(|_| SessionError::LimitReached)?
            } else {
                0
            };
            if required_count > event_limit
                || new_len
                    .checked_add(reserved_bytes)
                    .is_none_or(|size| size > byte_limit)
            {
                return Err(SessionError::LimitReached);
            }
        }
        let write_result = self
            .file
            .seek(SeekFrom::End(0))
            .and_then(|_| self.file.write_all(&bytes))
            .and_then(|()| self.sync_journal());
        if let Err(error) = write_result {
            self.poisoned = true;
            return Err(SessionError::Io(error));
        }
        Ok(bytes)
    }

    /// Sync the written bytes. The test hook models failure after a real write.
    fn sync_journal(&mut self) -> io::Result<()> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_sync) {
            return Err(io::Error::other("injected journal sync failure"));
        }
        self.file.sync_all()
    }

    /// Advance accounting only after the append and sync both succeed.
    fn commit_accounting(&mut self, bytes: Vec<u8>) -> Result<(), SessionError> {
        self.bytes = self
            .bytes
            .checked_add(u64::try_from(bytes.len()).map_err(|_| SessionError::LimitReached)?)
            .ok_or(SessionError::LimitReached)?;
        self.next_seq = self
            .next_seq
            .checked_add(1)
            .ok_or(SessionError::LimitReached)?;
        self.snapshot.event_count = self
            .snapshot
            .event_count
            .checked_add(1)
            .ok_or(SessionError::LimitReached)?;
        Ok(())
    }
}

#[derive(Serialize)]
struct EventRecord<'a, T> {
    schema_version: u32,
    session_id: Uuid,
    event_id: Uuid,
    seq: u64,
    timestamp: String,
    #[serde(rename = "type")]
    event_type: &'a str,
    payload: &'a T,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventEnvelope {
    schema_version: u32,
    session_id: Uuid,
    event_id: Uuid,
    seq: u64,
    timestamp: String,
    #[serde(rename = "type")]
    event_type: String,
    payload: Value,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SessionStarted {
    title: String,
    workspace: NativePath,
    workspace_id: Uuid,
    app_version: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MessagePayload {
    role: MessageRole,
    source: MessageSource,
    text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    call_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum MessageSource {
    UserCli,
    Provider,
    CoreTool,
}

/// A model-requested tool call before core dispatch. Schema version 2 only.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ToolRequestedPayload {
    request_id: Uuid,
    call_id: String,
    tool: String,
    arguments: String,
}

/// The core-observed result of one requested tool call.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    /// The broker completed the operation and the result is available.
    Succeeded,
    /// The request failed without an unresolved mutation.
    Failed,
    /// Current policy or native checks denied the operation.
    Denied,
}

/// Durable metadata for one core-observed tool result. Schema version 2 only.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ToolCompletedPayload {
    request_id: Uuid,
    call_id: String,
    outcome: ToolOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    output_bytes: u64,
    output_sha256: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TurnPayload {
    request_id: Uuid,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TurnFinishedPayload {
    request_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    code: Option<String>,
}

/// A bounded snapshot of the visible inputs and model profile for one request.
///
/// Values are untrusted historical metadata. In particular, `base_url` and
/// `model` do not authorize network access, and prompt text does not set policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestManifest {
    /// The compiled base prompt visible to the request.
    pub base_prompt: String,
    /// The version label for the compiled base prompt.
    pub prompt_version: String,
    /// SHA-256 digest of the compiled base prompt.
    pub prompt_sha256: String,
    /// User-selected custom instructions visible to the request.
    pub custom_instructions: String,
    /// The model name used for the request.
    pub model: String,
    /// The configured provider base URL.
    pub base_url: String,
    /// The configured input context limit.
    pub context_tokens: u64,
    /// The configured output limit.
    pub max_output_tokens: u32,
    /// Tool schema revision advertised to the model. Schema version 2 only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_schema_version: Option<String>,
    /// Exact tool names advertised to the model. Schema version 2 only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ManifestPayload {
    request_id: Uuid,
    manifest: RequestManifest,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EmptyPayload {}

/// Bound retained titles and refuse titles with no visible text.
fn validate_title(title: &str) -> Result<(), SessionError> {
    if title.trim().is_empty() || title.len() > MAX_TITLE_BYTES {
        return Err(SessionError::InvalidValue(
            "title must be 1 to 256 UTF-8 bytes",
        ));
    }
    Ok(())
}

/// Bound decoded fields before JSON encoding applies the separate wire limit.
fn validate_manifest(manifest: &RequestManifest, schema_version: u32) -> Result<(), SessionError> {
    let fields = [
        manifest.base_prompt.len(),
        manifest.prompt_version.len(),
        manifest.prompt_sha256.len(),
        manifest.custom_instructions.len(),
        manifest.model.len(),
        manifest.base_url.len(),
    ];
    let mut total = fields
        .into_iter()
        .try_fold(0_usize, |total, size| total.checked_add(size))
        .ok_or(SessionError::InvalidValue(
            "request manifest exceeds event size limit",
        ))?;
    match (&manifest.tool_schema_version, &manifest.tools) {
        (None, None) => {}
        (Some(version), Some(tools)) if schema_version >= SCHEMA_VERSION => {
            if version.is_empty()
                || version.len() > MAX_TOOL_SCHEMA_VERSION_BYTES
                || version.chars().any(char::is_control)
            {
                return Err(SessionError::InvalidValue("tool schema version is invalid"));
            }
            if tools.is_empty() || tools.len() > MAX_TOOL_NAMES {
                return Err(SessionError::InvalidValue("tool name list is invalid"));
            }
            total = total
                .checked_add(version.len())
                .ok_or(SessionError::InvalidValue(
                    "request manifest exceeds event size limit",
                ))?;
            let mut names = HashSet::new();
            for name in tools {
                validate_tool_name(name)?;
                if !names.insert(name) {
                    return Err(SessionError::InvalidValue("duplicate tool name"));
                }
                total = total
                    .checked_add(name.len())
                    .ok_or(SessionError::InvalidValue(
                        "request manifest exceeds event size limit",
                    ))?;
            }
        }
        _ => {
            return Err(SessionError::InvalidValue(
                "tool manifest fields require schema version 2 and must be both present or absent",
            ));
        }
    }
    if total > MAX_EVENT_BYTES {
        return Err(SessionError::InvalidValue(
            "request manifest exceeds event size limit",
        ));
    }
    Ok(())
}

/// Bound a provider call identifier without interpreting it as authority.
fn validate_tool_call_id(call_id: &str) -> Result<(), SessionError> {
    if call_id.is_empty()
        || call_id.len() > MAX_TOOL_CALL_ID_BYTES
        || call_id.chars().any(char::is_control)
    {
        Err(SessionError::InvalidValue(
            "tool call identifier is invalid or exceeds 128 bytes",
        ))
    } else {
        Ok(())
    }
}

/// Accept a bounded tool-name spelling; the runtime enforces the supported set.
fn validate_tool_name(name: &str) -> Result<(), SessionError> {
    if name.is_empty()
        || name.len() > MAX_TOOL_NAME_BYTES
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
    {
        return Err(SessionError::InvalidValue(
            "tool name is invalid or exceeds 64 bytes",
        ));
    }
    Ok(())
}

/// Require strict JSON object arguments within the bounded intent record.
fn validate_tool_arguments(arguments: &str) -> Result<(), SessionError> {
    if arguments.is_empty() || arguments.len() > MAX_TOOL_ARGUMENTS_BYTES {
        return Err(SessionError::InvalidValue(
            "tool arguments are empty or exceed 4 KiB",
        ));
    }
    let value = storage::parse_strict_json(arguments.as_bytes())
        .map_err(|_| SessionError::InvalidValue("tool arguments are not strict JSON"))?;
    if !value.is_object() {
        return Err(SessionError::InvalidValue(
            "tool arguments must be an object",
        ));
    }
    Ok(())
}

/// Require a stable outcome code and a bounded output identity.
fn validate_tool_outcome(outcome: ToolOutcome, code: Option<&str>) -> Result<(), SessionError> {
    match (outcome, code) {
        (ToolOutcome::Succeeded, None) => {}
        (ToolOutcome::Succeeded, Some(_)) => {
            return Err(SessionError::InvalidValue(
                "successful tool result cannot carry an error code",
            ));
        }
        (ToolOutcome::Failed | ToolOutcome::Denied, Some(code)) => {
            if code.is_empty()
                || code.len() > MAX_TOOL_ERROR_CODE_BYTES
                || code.chars().any(char::is_control)
            {
                return Err(SessionError::InvalidValue(
                    "tool error code is invalid or exceeds 128 bytes",
                ));
            }
        }
        (ToolOutcome::Failed | ToolOutcome::Denied, None) => {
            return Err(SessionError::InvalidValue(
                "failed or denied tool result requires an error code",
            ));
        }
    }
    Ok(())
}

/// Require a nonempty bounded rendering and a lowercase SHA-256 digest.
fn validate_tool_output(output_bytes: u64, output_sha256: &str) -> Result<(), SessionError> {
    if output_bytes == 0
        || output_bytes
            > u64::try_from(MAX_MESSAGE_BYTES).map_err(|_| SessionError::LimitReached)?
        || output_sha256.len() != MAX_TOOL_OUTPUT_DIGEST_BYTES
        || !output_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(SessionError::InvalidValue(
            "tool output size or digest is invalid",
        ));
    }
    Ok(())
}

/// Return a lowercase SHA-256 digest for bounded session content.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

/// Normalize event clocks to UTC; sequence numbers remain the ordering authority.
fn format_timestamp(timestamp: OffsetDateTime) -> Result<String, SessionError> {
    timestamp
        .to_offset(UtcOffset::UTC)
        .format(&Rfc3339)
        .map_err(|_| SessionError::InvalidValue("timestamp cannot be formatted"))
}

/// Include JSON escape expansion and the final LF in the event byte quota.
fn encode_event<T: Serialize>(event: &EventRecord<'_, T>) -> Result<Vec<u8>, SessionError> {
    let mut bytes = serde_json::to_vec(event)
        .map_err(|_| SessionError::InvalidValue("event cannot be serialized"))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_EVENT_BYTES {
        return Err(SessionError::LimitReached);
    }
    Ok(bytes)
}

/// Read at most the ordinary quota plus one terminal recovery record.
fn read_bounded(file: &mut File) -> Result<Vec<u8>, SessionError> {
    file.seek(SeekFrom::Start(0))?;
    let read_limit = u64::try_from(MAX_JOURNAL_BYTES + TERMINAL_RESERVE_BYTES + 1)
        .map_err(|_| SessionError::LimitReached)?;
    let mut contents = Vec::new();
    file.take(read_limit).read_to_end(&mut contents)?;
    if contents.len() > MAX_JOURNAL_BYTES + TERMINAL_RESERVE_BYTES {
        return Err(SessionError::InvalidJournal(
            "journal exceeds bounded recovery capacity",
        ));
    }
    Ok(contents)
}

/// Validate every event and transition without interpreting content as work.
fn replay(
    contents: &[u8],
    filename_id: Uuid,
) -> Result<(SessionSnapshot, ToolState), SessionError> {
    if contents.is_empty() || contents[0..contents.len().min(3)] == [0xef, 0xbb, 0xbf] {
        return Err(SessionError::InvalidJournal(
            "journal is empty or has a byte-order mark",
        ));
    }
    if contents.last() != Some(&b'\n') {
        return Err(SessionError::InvalidJournal(
            "journal has an incomplete final line",
        ));
    }
    let mut session_id = None;
    let mut schema_version = None;
    let mut event_ids = HashSet::new();
    let mut snapshot: Option<SessionSnapshot> = None;
    let mut tool_state = ToolState::default();
    let mut expected_seq = 0_u64;
    let mut total_events = 0_usize;
    let mut consumed_bytes = 0_usize;
    for raw_line in contents.split_inclusive(|byte| *byte == b'\n') {
        if raw_line.len() > MAX_EVENT_BYTES {
            return Err(SessionError::InvalidJournal("event exceeds 64 KiB"));
        }
        let mut line = &raw_line[..raw_line.len() - 1];
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        if line.is_empty() {
            return Err(SessionError::InvalidJournal("blank event line"));
        }
        let value: Value = storage::parse_strict_json(line).map_err(|_| {
            SessionError::InvalidJournal("event JSON is malformed or has duplicate keys")
        })?;
        let envelope: EventEnvelope = serde_json::from_value(value)
            .map_err(|_| SessionError::InvalidJournal("event envelope is invalid"))?;
        let end = consumed_bytes
            .checked_add(raw_line.len())
            .ok_or(SessionError::LimitReached)?;
        if (end > MAX_JOURNAL_BYTES || total_events >= MAX_EVENTS)
            && (!matches!(
                envelope.event_type.as_str(),
                "turn.completed" | "turn.failed"
            ) || !snapshot
                .as_ref()
                .is_some_and(|state| state.active_turn.is_some())
                || consumed_bytes > MAX_JOURNAL_BYTES
                || total_events > MAX_EVENTS
                || raw_line.len() > TERMINAL_RESERVE_BYTES)
        {
            return Err(SessionError::InvalidJournal(
                "only one terminal recovery event may exceed ordinary quotas",
            ));
        }
        consumed_bytes = end;
        if !matches!(envelope.schema_version, SCHEMA_VERSION_V1 | SCHEMA_VERSION) {
            return Err(SessionError::InvalidJournal("unsupported schema version"));
        }
        if schema_version.is_some_and(|prior| prior != envelope.schema_version) {
            return Err(SessionError::InvalidJournal(
                "journal mixes schema versions",
            ));
        }
        schema_version = Some(envelope.schema_version);
        if envelope.session_id != filename_id {
            return Err(SessionError::InvalidJournal(
                "event session UUID does not match filename",
            ));
        }
        if session_id.is_some_and(|prior| prior != envelope.session_id) {
            return Err(SessionError::InvalidJournal("conflicting session UUIDs"));
        }
        session_id = Some(envelope.session_id);
        if envelope.seq != expected_seq {
            return Err(SessionError::InvalidJournal(
                "event sequence is not contiguous",
            ));
        }
        if !event_ids.insert(envelope.event_id) {
            return Err(SessionError::InvalidJournal("duplicate event UUID"));
        }
        validate_timestamp(&envelope.timestamp)?;
        match envelope.event_type.as_str() {
            "session.started" => {
                if snapshot.is_some() || expected_seq != 0 {
                    return Err(SessionError::InvalidJournal(
                        "session.started is not the first event",
                    ));
                }
                let started: SessionStarted = decode_payload(envelope.payload)?;
                validate_title(&started.title)
                    .map_err(|_| SessionError::InvalidJournal("initial title is invalid"))?;
                snapshot = Some(SessionSnapshot {
                    schema_version: envelope.schema_version,
                    session_id: envelope.session_id,
                    workspace_id: started.workspace_id,
                    archived: false,
                    active_turn: None,
                    messages: Vec::new(),
                    title: started.title,
                    workspace: started.workspace,
                    event_count: 0,
                });
            }
            "message.recorded" => {
                let target = snapshot
                    .as_mut()
                    .ok_or(SessionError::InvalidJournal("event before start"))?;
                ensure_not_archived(target)?;
                let message: MessagePayload = decode_payload(envelope.payload)?;
                if message.text.len() > MAX_MESSAGE_BYTES {
                    return Err(SessionError::InvalidJournal("message exceeds 16 KiB"));
                }
                match (message.role, message.source) {
                    (MessageRole::User, MessageSource::UserCli)
                    | (MessageRole::Assistant, MessageSource::Provider) => {
                        if message.call_id.is_some() {
                            return Err(SessionError::InvalidJournal(
                                "non-tool message has a call identifier",
                            ));
                        }
                    }
                    (MessageRole::Tool, MessageSource::CoreTool) => {
                        if envelope.schema_version < SCHEMA_VERSION {
                            return Err(SessionError::InvalidJournal(
                                "tool messages require schema version 2",
                            ));
                        }
                        if target.active_turn.is_none() {
                            return Err(SessionError::InvalidJournal(
                                "tool message has no active turn",
                            ));
                        }
                        let call_id =
                            message
                                .call_id
                                .as_deref()
                                .ok_or(SessionError::InvalidJournal(
                                    "tool message has no call identifier",
                                ))?;
                        validate_tool_call_id(call_id).map_err(|_| {
                            SessionError::InvalidJournal("tool message call identifier is invalid")
                        })?;
                        if !tool_state.completed.contains(call_id)
                            || tool_state.messaged.contains(call_id)
                        {
                            return Err(SessionError::InvalidJournal(
                                "tool message has no completed, unmessaged call",
                            ));
                        }
                        let expected =
                            tool_state
                                .outputs
                                .get(call_id)
                                .ok_or(SessionError::InvalidJournal(
                                    "tool message has no recorded output identity",
                                ))?;
                        let text_bytes = u64::try_from(message.text.len())
                            .map_err(|_| SessionError::LimitReached)?;
                        if expected.0 != text_bytes
                            || expected.1 != sha256_hex(message.text.as_bytes())
                        {
                            return Err(SessionError::InvalidJournal(
                                "tool message does not match its recorded output identity",
                            ));
                        }
                        tool_state.outputs.remove(call_id);
                        tool_state.messaged.insert(call_id.to_owned());
                    }
                    _ => {
                        return Err(SessionError::InvalidJournal(
                            "message role and source do not match",
                        ));
                    }
                }
                target.messages.push(RecordedMessage {
                    role: message.role,
                    text: message.text,
                });
            }
            "turn.started" => {
                let target = snapshot
                    .as_mut()
                    .ok_or(SessionError::InvalidJournal("event before start"))?;
                ensure_not_archived(target)?;
                if target.active_turn.is_some() {
                    return Err(SessionError::InvalidJournal("nested active turn"));
                }
                let payload: TurnPayload = decode_payload(envelope.payload)?;
                tool_state.clear();
                target.active_turn = Some(payload.request_id);
            }
            "tool.requested" => {
                if envelope.schema_version < SCHEMA_VERSION {
                    return Err(SessionError::InvalidJournal(
                        "tool events require schema version 2",
                    ));
                }
                let target = snapshot
                    .as_ref()
                    .ok_or(SessionError::InvalidJournal("event before start"))?;
                if target.archived || target.active_turn.is_none() {
                    return Err(SessionError::InvalidJournal(
                        "tool request has no active turn",
                    ));
                }
                let payload: ToolRequestedPayload = decode_payload(envelope.payload)?;
                if target.active_turn != Some(payload.request_id) {
                    return Err(SessionError::InvalidJournal(
                        "tool request does not match active turn",
                    ));
                }
                validate_tool_call_id(&payload.call_id).map_err(|_| {
                    SessionError::InvalidJournal("tool request call identifier is invalid")
                })?;
                validate_tool_name(&payload.tool)
                    .map_err(|_| SessionError::InvalidJournal("tool request name is invalid"))?;
                validate_tool_arguments(&payload.arguments).map_err(|_| {
                    SessionError::InvalidJournal("tool request arguments are invalid")
                })?;
                if tool_state.pending.contains(&payload.call_id)
                    || tool_state.completed.contains(&payload.call_id)
                {
                    return Err(SessionError::InvalidJournal(
                        "duplicate tool call identifier",
                    ));
                }
                tool_state.pending.insert(payload.call_id);
            }
            "tool.completed" => {
                if envelope.schema_version < SCHEMA_VERSION {
                    return Err(SessionError::InvalidJournal(
                        "tool events require schema version 2",
                    ));
                }
                let target = snapshot
                    .as_ref()
                    .ok_or(SessionError::InvalidJournal("event before start"))?;
                if target.archived || target.active_turn.is_none() {
                    return Err(SessionError::InvalidJournal(
                        "tool result has no active turn",
                    ));
                }
                let payload: ToolCompletedPayload = decode_payload(envelope.payload)?;
                if target.active_turn != Some(payload.request_id) {
                    return Err(SessionError::InvalidJournal(
                        "tool result does not match active turn",
                    ));
                }
                validate_tool_call_id(&payload.call_id).map_err(|_| {
                    SessionError::InvalidJournal("tool result call identifier is invalid")
                })?;
                if !tool_state.pending.contains(&payload.call_id) {
                    return Err(SessionError::InvalidJournal(
                        "tool result has no pending request",
                    ));
                }
                validate_tool_outcome(payload.outcome, payload.code.as_deref())
                    .map_err(|_| SessionError::InvalidJournal("tool result outcome is invalid"))?;
                validate_tool_output(payload.output_bytes, &payload.output_sha256).map_err(
                    |_| SessionError::InvalidJournal("tool result output metadata is invalid"),
                )?;
                tool_state.pending.remove(&payload.call_id);
                tool_state.completed.insert(payload.call_id.clone());
                tool_state.outputs.insert(
                    payload.call_id,
                    (payload.output_bytes, payload.output_sha256),
                );
            }
            "request.manifest" => {
                let target = snapshot
                    .as_ref()
                    .ok_or(SessionError::InvalidJournal("event before start"))?;
                if target.active_turn.is_none() || target.archived {
                    return Err(SessionError::InvalidJournal("manifest has no active turn"));
                }
                let payload: ManifestPayload = decode_payload(envelope.payload)?;
                if target.active_turn != Some(payload.request_id) {
                    return Err(SessionError::InvalidJournal(
                        "manifest does not match active turn",
                    ));
                }
                validate_manifest(&payload.manifest, envelope.schema_version).map_err(|_| {
                    SessionError::InvalidJournal("request manifest exceeds event size limit")
                })?;
            }
            "turn.completed" | "turn.failed" => {
                let target = snapshot
                    .as_mut()
                    .ok_or(SessionError::InvalidJournal("event before start"))?;
                let payload: TurnFinishedPayload = decode_payload(envelope.payload)?;
                if target.active_turn != Some(payload.request_id) {
                    return Err(SessionError::InvalidJournal(
                        "turn completion does not match active turn",
                    ));
                }
                if envelope.event_type == "turn.failed" {
                    let code = payload
                        .code
                        .as_deref()
                        .ok_or(SessionError::InvalidJournal("failed turn has no code"))?;
                    if code.is_empty()
                        || code.len() > MAX_FAILURE_CODE_BYTES
                        || code.chars().any(char::is_control)
                    {
                        return Err(SessionError::InvalidJournal("failure code is invalid"));
                    }
                } else if payload.code.is_some() {
                    return Err(SessionError::InvalidJournal(
                        "completed turn has a failure code",
                    ));
                } else if !tool_state.pending.is_empty()
                    || tool_state.completed != tool_state.messaged
                    || !tool_state.outputs.is_empty()
                {
                    return Err(SessionError::InvalidJournal(
                        "completed turn has unresolved or unrecorded tool calls",
                    ));
                }
                target.active_turn = None;
                tool_state.clear();
            }
            "session.archived" => {
                let target = snapshot
                    .as_mut()
                    .ok_or(SessionError::InvalidJournal("event before start"))?;
                let _: EmptyPayload = decode_payload(envelope.payload)?;
                if target.active_turn.is_some() {
                    return Err(SessionError::InvalidJournal("active session was archived"));
                }
                target.archived = true;
                tool_state.clear();
            }
            "session.restored" => {
                let target = snapshot
                    .as_mut()
                    .ok_or(SessionError::InvalidJournal("event before start"))?;
                let _: EmptyPayload = decode_payload(envelope.payload)?;
                target.archived = false;
            }
            _ => return Err(SessionError::InvalidJournal("unknown event type")),
        }
        total_events = total_events
            .checked_add(1)
            .ok_or(SessionError::LimitReached)?;
        if total_events > MAX_EVENTS + 1 {
            return Err(SessionError::InvalidJournal(
                "journal exceeds 10,000 events",
            ));
        }
        expected_seq = expected_seq
            .checked_add(1)
            .ok_or(SessionError::LimitReached)?;
        if let Some(target) = snapshot.as_mut() {
            target.event_count =
                u64::try_from(total_events).map_err(|_| SessionError::LimitReached)?;
        }
    }
    let snapshot = snapshot.ok_or(SessionError::InvalidJournal(
        "missing session.started event",
    ))?;
    Ok((snapshot, tool_state))
}

/// Reject transcript changes while replay says the session is archived.
fn ensure_not_archived(snapshot: &SessionSnapshot) -> Result<(), SessionError> {
    if snapshot.archived {
        Err(SessionError::InvalidJournal(
            "event mutates an archived session",
        ))
    } else {
        Ok(())
    }
}

/// Require parseable UTC metadata without using clocks to reorder events.
fn validate_timestamp(value: &str) -> Result<(), SessionError> {
    let timestamp = OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|_| SessionError::InvalidJournal("timestamp is not RFC 3339"))?;
    if timestamp.offset().whole_seconds() != 0 {
        return Err(SessionError::InvalidJournal("timestamp is not UTC"));
    }
    Ok(())
}

/// Apply the selected event's typed schema and reject unsupported fields.
fn decode_payload<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, SessionError> {
    serde_json::from_value(value)
        .map_err(|_| SessionError::InvalidJournal("event payload is invalid"))
}

/// Preserve storage error categories at the session journal boundary.
fn map_storage_error(error: StorageError) -> SessionError {
    match error {
        StorageError::Io(error) => SessionError::Io(error),
        StorageError::Invalid(reason) => SessionError::InvalidJournal(reason),
        StorageError::Busy => SessionError::Busy,
    }
}
#[cfg(test)]
mod tests {
    use super::{MessageRole, SessionError, SessionJournal};
    use std::fs::{self, File};
    use std::io::{Read, Seek, SeekFrom};

    #[test]
    fn create_with_workspace_id_preserves_metadata_across_sessions_and_reopen() {
        let temporary = tempfile::tempdir().expect("private temporary directory");
        let workspace = temporary.path().join("workspace");
        fs::create_dir(&workspace).expect("workspace directory");
        let workspace_id: crate::policy::WorkspaceId = uuid::Uuid::new_v4()
            .to_string()
            .parse()
            .expect("valid generated workspace identifier");
        let expected_id = *workspace_id.as_uuid();
        let data_root = temporary.path().join("app-data");
        let first = SessionJournal::create_with_workspace_id(
            &data_root,
            &workspace,
            "First metadata test",
            workspace_id,
        )
        .expect("create first journal with workspace metadata");
        let second = SessionJournal::create_with_workspace_id(
            &data_root,
            &workspace,
            "Second metadata test",
            workspace_id,
        )
        .expect("create second journal with workspace metadata");
        let paths = [first.path().to_path_buf(), second.path().to_path_buf()];
        assert_eq!(first.snapshot.workspace_id, expected_id);
        assert_eq!(second.snapshot.workspace_id, expected_id);
        drop(first);
        drop(second);

        for path in paths {
            let reopened = SessionJournal::open(&path).expect("reopen journal");
            assert_eq!(reopened.snapshot.workspace_id, expected_id);
            assert_eq!(reopened.snapshot.active_turn, None);
        }
    }
    #[test]
    fn append_failure_preserves_snapshot_and_poisons_future_mutations() {
        let temporary = tempfile::tempdir().expect("private temporary directory");
        let workspace = temporary.path().join("workspace");
        fs::create_dir(&workspace).expect("workspace directory");
        let mut journal = SessionJournal::create(
            &temporary.path().join("app-data"),
            &workspace,
            "Failure test",
        )
        .expect("create journal");
        let original_snapshot = journal.snapshot.clone();
        let mut original_bytes = Vec::new();
        journal
            .file
            .seek(SeekFrom::Start(0))
            .expect("seek in locked journal");
        journal
            .file
            .read_to_end(&mut original_bytes)
            .expect("read through locked handle");
        let path = journal.path().to_path_buf();

        // Replace the owned handle with a read-only one to force a write error.
        journal.file = File::open(path).expect("read-only journal handle");
        assert!(matches!(
            journal.record_message(MessageRole::User, "not persisted"),
            Err(SessionError::Io(_))
        ));

        assert_eq!(journal.snapshot, original_snapshot);
        assert_eq!(
            fs::read(journal.path()).expect("journal remains unchanged"),
            original_bytes
        );
        assert!(matches!(journal.start_turn(), Err(SessionError::Poisoned)));
    }

    #[test]
    fn sync_failure_after_write_preserves_snapshot_and_poisons_writer() {
        let temporary = tempfile::tempdir().expect("private test directory");
        let mut journal = SessionJournal::create(
            &temporary.path().join("data"),
            temporary.path(),
            "sync failure",
        )
        .expect("journal");
        let snapshot = journal.snapshot.clone();
        let original_len = journal.file.metadata().expect("metadata").len();
        journal.fail_next_sync = true;
        assert!(matches!(
            journal.record_message(MessageRole::User, "written but uncertain"),
            Err(SessionError::Io(_))
        ));
        assert!(
            journal.file.metadata().expect("metadata").len() > original_len,
            "write must precede sync failure"
        );
        assert_eq!(journal.snapshot, snapshot);
        assert!(matches!(journal.start_turn(), Err(SessionError::Poisoned)));
    }

    #[test]
    fn active_turn_keeps_an_event_slot_and_bytes_for_termination() {
        for by_bytes in [false, true] {
            let temporary = tempfile::tempdir().expect("private test directory");
            let mut journal = SessionJournal::create(
                &temporary.path().join("data"),
                temporary.path(),
                "capacity",
            )
            .expect("journal");
            let initial_bytes = journal.bytes;
            let initial_count = journal.next_seq;
            if by_bytes {
                journal.bytes =
                    u64::try_from(super::MAX_JOURNAL_BYTES - super::TERMINAL_RESERVE_BYTES)
                        .expect("size");
            } else {
                journal.next_seq = u64::try_from(super::MAX_EVENTS - 1).expect("count");
            }
            let idle = journal.snapshot.clone();
            assert!(
                matches!(journal.start_turn(), Err(SessionError::LimitReached)),
                "starting must also reserve its terminal capacity"
            );
            assert_eq!(journal.snapshot, idle);
            journal.bytes = initial_bytes;
            journal.next_seq = initial_count;
            let request_id = journal.start_turn().expect("start turn");
            // Isolate accounting boundaries without thousands of sync operations.
            if by_bytes {
                journal.bytes =
                    u64::try_from(super::MAX_JOURNAL_BYTES - super::TERMINAL_RESERVE_BYTES)
                        .expect("size");
            } else {
                journal.next_seq = u64::try_from(super::MAX_EVENTS - 1).expect("count");
            }
            let before = journal.snapshot.clone();
            assert!(matches!(
                journal.record_message(MessageRole::Assistant, "no room"),
                Err(SessionError::LimitReached)
            ));
            assert_eq!(journal.snapshot, before);
            journal
                .fail_turn(request_id, "operator_interrupted")
                .expect("reserved terminal capacity");
            assert_eq!(journal.snapshot.active_turn, None);
        }
    }
}
