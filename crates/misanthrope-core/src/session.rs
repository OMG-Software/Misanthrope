//! Bounded, single-writer JSONL persistence for a Misanthrope session.
//!
//! Journals are untrusted input when opened. This module validates the complete
//! file before returning a writable handle. It does not execute or resume work.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};
use uuid::Uuid;

const SCHEMA_VERSION: u32 = 1;
const MAX_TITLE_BYTES: usize = 256;
const MAX_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_EVENT_BYTES: usize = 64 * 1024;
const MAX_JOURNAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_EVENTS: usize = 10_000;
const MAX_FAILURE_CODE_BYTES: usize = 128;
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
    /// The UUID shared by all events in this journal.
    pub session_id: Uuid,
    /// Stable identifier for the workspace association recorded at creation.
    pub workspace_id: Uuid,
    /// Whether the session is archived.
    pub archived: bool,
    /// The currently active turn, if any. Opening never resumes it.
    pub active_turn: Option<Uuid>,
    /// User and assistant messages in append order.
    pub messages: Vec<RecordedMessage>,
    /// Current session title.
    pub title: String,
    /// Canonical workspace path encoded as native units.
    pub workspace: NativePath,
    /// Number of validated events, including the initial event.
    pub event_count: u64,
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
    /// access. The session event is synced before this function returns.
    pub fn create(data_root: &Path, workspace: &Path, title: &str) -> Result<Self, SessionError> {
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
        create_private_directories(&dir)?;
        let filename = format!(
            "misanthrope-{:02}-{:02}-{:02}-{:03}Z-{session_id}.jsonl",
            now.hour(),
            now.minute(),
            now.second(),
            now.millisecond()
        );
        let path = dir.join(filename);
        let mut create_options = OpenOptions::new();
        create_options.read(true).write(true).create_new(true);
        configure_journal_options(&mut create_options);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            create_options.mode(0o600);
        }
        let mut file = create_options.open(&path)?;
        check_open_file(&path, &file)?;
        file.try_lock().map_err(map_lock_error)?;

        let started = SessionStarted {
            title: title.to_owned(),
            workspace,
            workspace_id: Uuid::new_v4(),
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
        sync_parent_directory(&dir)?;

        Ok(Self {
            path,
            file,
            snapshot: SessionSnapshot {
                session_id,
                workspace_id: started.workspace_id,
                archived: false,
                active_turn: None,
                messages: Vec::new(),
                title: title.to_owned(),
                workspace: started.workspace,
                event_count: 1,
            },
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
            .strip_prefix("misanthrope-")
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
        let mut file = open_existing(path)?;
        check_open_file(path, &file)?;
        file.try_lock().map_err(map_lock_error)?;
        check_link_count(&file)?;
        let contents = read_bounded(&mut file)?;
        let snapshot = replay(&contents, filename_id)?;
        let bytes = u64::try_from(contents.len()).map_err(|_| SessionError::LimitReached)?;
        let next_seq = snapshot.event_count;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            snapshot,
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
        validate_manifest(manifest)?;
        let request_id = Uuid::nil();
        let mut seq = self.next_seq;
        let mut size = self.bytes;
        // A nine-digit fractional timestamp bounds any timestamp written later.
        let timestamp = "2026-11-08T00:00:00.999999999Z";
        let mut measure = |kind, payload: Value| -> Result<(), SessionError> {
            let encoded = encode_event(&EventRecord {
                schema_version: SCHEMA_VERSION,
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
        };
        let payload = MessagePayload {
            role,
            source,
            text: text.to_owned(),
        };
        let bytes = self.append("message.recorded", &payload)?;
        self.snapshot.messages.push(RecordedMessage {
            role,
            text: text.to_owned(),
        });
        self.commit_accounting(bytes)?;
        Ok(())
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
        validate_manifest(&manifest)?;
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
        let payload = TurnFinishedPayload {
            request_id,
            code: code.map(str::to_owned),
        };
        let bytes = self.append(event_type, &payload)?;
        self.snapshot.active_turn = None;
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
            schema_version: SCHEMA_VERSION,
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
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum MessageSource {
    UserCli,
    Provider,
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
fn validate_manifest(manifest: &RequestManifest) -> Result<(), SessionError> {
    let fields = [
        manifest.base_prompt.len(),
        manifest.prompt_version.len(),
        manifest.prompt_sha256.len(),
        manifest.custom_instructions.len(),
        manifest.model.len(),
        manifest.base_url.len(),
    ];
    let total = fields
        .into_iter()
        .try_fold(0_usize, |total, size| total.checked_add(size));
    if total.is_none_or(|size| size > MAX_EVENT_BYTES) {
        return Err(SessionError::InvalidValue(
            "request manifest exceeds event size limit",
        ));
    }
    Ok(())
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
fn replay(contents: &[u8], filename_id: Uuid) -> Result<SessionSnapshot, SessionError> {
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
    let mut event_ids = HashSet::new();
    let mut snapshot: Option<SessionSnapshot> = None;
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
        let value: Value = parse_strict_json(line)?;
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
        if envelope.schema_version != SCHEMA_VERSION {
            return Err(SessionError::InvalidJournal("unsupported schema version"));
        }
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
                if !matches!(
                    (message.role, message.source),
                    (MessageRole::User, MessageSource::UserCli)
                        | (MessageRole::Assistant, MessageSource::Provider)
                ) {
                    return Err(SessionError::InvalidJournal(
                        "message role and source do not match",
                    ));
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
                target.active_turn = Some(payload.request_id);
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
                validate_manifest(&payload.manifest).map_err(|_| {
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
                }
                target.active_turn = None;
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
    snapshot.ok_or(SessionError::InvalidJournal(
        "missing session.started event",
    ))
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

/// Reject duplicate keys at every depth and require exactly one JSON value.
fn parse_strict_json(bytes: &[u8]) -> Result<Value, SessionError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = StrictValue::deserialize(&mut deserializer).map_err(|_| {
        SessionError::InvalidJournal("event JSON is malformed or has duplicate keys")
    })?;
    deserializer
        .end()
        .map_err(|_| SessionError::InvalidJournal("event JSON has trailing data"))?;
    Ok(value.0)
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct StrictVisitor;
        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = StrictValue;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON value without duplicate object keys")
            }
            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Bool(value)))
            }
            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Number(Number::from(value))))
            }
            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Number(Number::from(value))))
            }
            fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Number::from_f64(value)
                    .map(|number| StrictValue(Value::Number(number)))
                    .ok_or_else(|| E::custom("non-finite number"))
            }
            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(StrictValue(Value::String(value.to_owned())))
            }
            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::String(value)))
            }
            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            /// Keep duplicate-key checks active inside nested array elements.
            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<StrictValue>()? {
                    values.push(value.0);
                }
                Ok(StrictValue(Value::Array(values)))
            }
            /// Detect repeated keys before converting an object to a JSON map.
            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut values = Map::new();
                while let Some((key, value)) = map.next_entry::<String, StrictValue>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom("duplicate JSON object key"));
                    }
                    values.insert(key, value.0);
                }
                Ok(StrictValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(StrictVisitor)
    }
}

/// Open with native link checks and Windows replacement protection enabled.
fn open_existing(path: &Path) -> Result<File, SessionError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    configure_journal_options(&mut options);
    Ok(options.open(path)?)
}

/// Keep Windows files at their path while the journal owner holds the handle.
fn configure_journal_options(options: &mut OpenOptions) {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ_AND_WRITE: u32 = 0x0000_0003;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options
            .share_mode(FILE_SHARE_READ_AND_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(not(windows))]
    {
        let _ = options;
    }
}

/// Validate the opened object and detect Unix path replacement during opening.
/// Parent directories must still be trusted; this is not a filesystem broker.
fn check_open_file(path: &Path, file: &File) -> Result<(), SessionError> {
    let opened = file.metadata()?;
    if !opened.is_file() {
        return Err(SessionError::InvalidJournal(
            "journal handle is not a regular file",
        ));
    }
    check_link_count(file)?;
    let path_metadata = fs::symlink_metadata(path)?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(SessionError::InvalidJournal(
            "journal path is a link or not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened.dev() != path_metadata.dev() || opened.ino() != path_metadata.ino() {
            return Err(SessionError::InvalidJournal(
                "journal path changed during open",
            ));
        }
    }
    Ok(())
}

/// Inspect the existing handle through the platform boundary, without reopening.
fn check_link_count(file: &File) -> Result<(), SessionError> {
    match misanthrope_platform::validate_journal_handle(file) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => Err(
            SessionError::InvalidJournal("journal handle is linked or not a regular file"),
        ),
        Err(error) => Err(SessionError::Io(error)),
    }
}

/// Distinguish an existing writer from an unavailable or failed locking facility.
fn map_lock_error(error: std::fs::TryLockError) -> SessionError {
    match error {
        std::fs::TryLockError::WouldBlock => SessionError::Busy,
        std::fs::TryLockError::Error(error) => SessionError::Io(error),
    }
}

/// Request private Unix modes for new directories; do not alter existing ACLs.
fn create_private_directories(path: &Path) -> Result<(), SessionError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(path)?;
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)?;
    }
    Ok(())
}

/// Sync the Unix directory entry; Windows directory durability is not qualified.
fn sync_parent_directory(directory: &Path) -> Result<(), SessionError> {
    #[cfg(unix)]
    {
        File::open(directory)?.sync_all()?;
    }
    #[cfg(windows)]
    {
        // std does not provide a portable safe directory-handle sync contract.
        let _ = directory;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{MessageRole, SessionError, SessionJournal};
    use std::fs::{self, File};
    use std::io::{Read, Seek, SeekFrom};

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
