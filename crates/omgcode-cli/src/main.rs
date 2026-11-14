//! Developer CLI for the first OMGCode core prototype.
#![forbid(unsafe_code)]

mod approvals_cli;
mod control_cli;
mod file_cli;
mod model_tools;
mod packages_cli;
mod run_cli;

use clap::{Args, Parser, Subcommand};
use directories::ProjectDirs;
use omgcode_core::control::ControlStore;
use omgcode_core::prompt::{
    self, BASE_PROMPT, BASE_PROMPT_VERSION, ConversationMessage, ConversationRole,
};
use omgcode_core::provider::{LocalModelClient, LocalModelConfig};
use omgcode_core::session::{
    MessageRole, NativePath, RequestManifest, SessionError, SessionJournal,
};
use omgcode_core::turn::replay_history;
use serde::Serialize;
use serde_json::json;
use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

/// Whether trusted runs and toolchain profiles exist on this platform.
const RUN_SUPPORTED: bool = cfg!(any(target_os = "linux", windows));

#[derive(Parser)]
#[command(name = "omgcode", version, about = "OMGCode local core prototype")]
struct Cli {
    /// Trusted application storage override. Keep this outside project folders.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Inspect the compiled, read-only base prompt and its digest.
    Prompt,
    /// Report the capabilities actually available in this prototype.
    Capabilities,
    /// Create, inspect, recover, archive, or restore a JSONL session.
    Session {
        #[command(subcommand)]
        action: SessionCommand,
    },
    /// Configure workspaces and inspect configuration-only permission policy.
    Workspace {
        #[command(subcommand)]
        action: control_cli::WorkspaceCommand,
    },
    /// Perform bounded, native file operations through trusted operator input.
    File {
        #[command(subcommand)]
        action: file_cli::FileCommand,
    },
    /// Send one turn to an explicitly selected local LM Studio model.
    Chat(ChatArgs),
    /// Run one program in a workspace under native confinement, using a
    /// recorded toolchain profile. Needs the workspace `execute` preference.
    Run(run_cli::RunArgs),
    /// Inspect, acknowledge, or archive run records.
    Runs {
        #[command(subcommand)]
        action: run_cli::RunsCommand,
    },
    /// Fetch a package from a declared registry into a private cache.
    Packages {
        #[command(subcommand)]
        action: packages_cli::PackagesCommand,
    },
    /// Inspect, decide, or consume trusted per-call approvals.
    Approvals {
        #[command(subcommand)]
        action: approvals_cli::ApprovalsCommand,
    },
}

#[derive(Subcommand)]
enum SessionCommand {
    New {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long, default_value = "New session")]
        title: String,
    },
    /// Read a selected journal without executing its recorded content.
    Inspect {
        #[command(flatten)]
        locator: JournalArgs,
    },
    Archive {
        #[command(flatten)]
        locator: JournalArgs,
    },
    Restore {
        #[command(flatten)]
        locator: JournalArgs,
    },
    /// Mark an abandoned turn failed without retrying its model request.
    Interrupt {
        #[command(flatten)]
        locator: JournalArgs,
    },
}

#[derive(Args)]
struct JournalArgs {
    #[arg(required_unless_present = "journal_native")]
    journal: Option<PathBuf>,
    /// Lossless journal_native JSON returned by an earlier command.
    #[arg(long, conflicts_with = "journal")]
    journal_native: Option<String>,
}

#[derive(Args)]
struct ChatArgs {
    #[arg(long, required_unless_present_any = ["journal", "journal_native"], conflicts_with_all = ["journal", "journal_native"])]
    workspace: Option<PathBuf>,
    /// Resume this explicit journal. Historical grants never authorize tools.
    #[arg(long)]
    journal: Option<PathBuf>,
    /// Resume using the lossless journal_native JSON from an earlier result.
    #[arg(long, conflicts_with = "journal")]
    journal_native: Option<String>,
    #[arg(long, default_value = "Local model chat")]
    title: String,
    #[arg(long, default_value = "http://127.0.0.1:1234/v1")]
    endpoint: String,
    #[arg(long)]
    model: String,
    /// Validated context capacity for the selected loaded model.
    #[arg(long)]
    context_tokens: u64,
    #[arg(long, default_value_t = 1024)]
    max_output_tokens: u32,
    /// User-level preferences. This cannot replace the compiled base prompt.
    #[arg(long, default_value = "")]
    instructions: String,
    /// Current user request. Quote it as one shell argument.
    message: String,
}

#[derive(Serialize)]
struct CliError {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<Box<JournalLocator>>,
}

#[derive(Clone, Serialize)]
struct JournalLocator {
    session_id: uuid::Uuid,
    journal: Option<String>,
    journal_display: String,
    journal_native: NativePath,
}

#[derive(Serialize)]
struct JournalOutput<T> {
    #[serde(flatten)]
    locator: JournalLocator,
    #[serde(flatten)]
    detail: T,
}

impl CliError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            session: None,
        }
    }

    /// Preserve the existing session locator on all post-open chat failures.
    fn with_session(mut self, session: JournalLocator) -> Self {
        self.session = Some(Box::new(session));
        self
    }
}

/// Emit controlled JSON errors; never render untrusted text as terminal escapes.
fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // A closed error stream cannot be repaired by recursively printing.
            if serde_json::to_writer(io::stderr().lock(), &json!({"error": error})).is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::FAILURE
        }
    }
}

/// Dispatch trusted operator commands without providing any model execution route.
/// `run` is an operator command; no model tool reaches it.
fn run(cli: Cli) -> Result<(), CliError> {
    match cli.command {
        Command::Prompt => print_json(&json!({
            "version": BASE_PROMPT_VERSION,
            "sha256": prompt::base_prompt_sha256(),
            "text": BASE_PROMPT,
        })),
        Command::Capabilities => print_json(&json!({
            "version": env!("CARGO_PKG_VERSION"),
            "prototype": true,
            "sessions_jsonl": true,
            "archive_restore": true,
            "compiled_prompt": true,
            "local_model_chat": true,
            "workspace_registry": true,
            "permission_policy": true,
            "permission_audit": true,
            "policy_decision": "configuration_only",
            "workspace_file_broker_implemented": cfg!(any(target_os = "linux", target_os = "windows")),
            "workspace_file_broker": if cfg!(any(target_os = "linux", target_os = "windows")) {
                serde_json::Value::Null
            } else {
                serde_json::Value::Bool(false)
            },
            "exclusive_file_creation": if cfg!(any(target_os = "linux", target_os = "windows")) {
                serde_json::Value::Null
            } else {
                serde_json::Value::Bool(false)
            },
            "existing_file_replacement": cfg!(any(target_os = "linux", target_os = "windows")),
            "checkpoint_restore": cfg!(any(target_os = "linux", target_os = "windows")),
            "checkpoint_deletion": cfg!(any(target_os = "linux", target_os = "windows")),
            "edit_previews": cfg!(any(target_os = "linux", target_os = "windows")),
            "model_read_list_tools": model_tools::MODEL_TOOLS_SUPPORTED,
            "model_command_tool": "approval_required",
            "model_write_tools": false,
            "workspace_container_grants": omgcode_core::control::CONTAINER_GRANTS_SUPPORTED,
            "workspace_file_tools": false,
            "toolchain_profiles": RUN_SUPPORTED,
            "package_network_profiles": true,
            "package_network_enforcement": false,
            "package_fetch": true,
            "provider_profiles": true,
            "provider_adapters": false,
            "approvals": true,
            "process_execution": if RUN_SUPPORTED {
                serde_json::Value::from("trusted_cli_only")
            } else {
                serde_json::Value::Bool(false)
            },
            "mcp": false,
            "memories": false,
            "sandbox_backend": if cfg!(target_os = "linux") {
                "landlock"
            } else if cfg!(windows) {
                "appcontainer"
            } else {
                "unavailable"
            },
            "tool_policy": if cfg!(any(target_os = "linux", target_os = "windows")) {
                "read_list_only_when_workspace_model_tools_enabled"
            } else {
                "reject all model-requested tools"
            },
        })),
        Command::Session { action } => match action {
            SessionCommand::New { workspace, title } => {
                let token = read_lm_token()?;
                let title = redact_known_secret(&title, token.as_deref());
                let data_root = data_root(cli.data_dir)?;
                let journal = create_session(&data_root, &workspace, &title)?;
                print_session(&journal)
            }
            SessionCommand::Inspect { locator } => {
                let path = resolve_journal(locator.journal, locator.journal_native)?;
                print_session(&SessionJournal::open(&path).map_err(session_error)?)
            }
            SessionCommand::Archive { locator } => {
                let path = resolve_journal(locator.journal, locator.journal_native)?;
                let mut journal = SessionJournal::open(&path).map_err(session_error)?;
                journal.archive().map_err(session_error)?;
                print_session(&journal)
            }
            SessionCommand::Restore { locator } => {
                let path = resolve_journal(locator.journal, locator.journal_native)?;
                let mut journal = SessionJournal::open(&path).map_err(session_error)?;
                journal.restore().map_err(session_error)?;
                print_session(&journal)
            }
            SessionCommand::Interrupt { locator } => {
                let path = resolve_journal(locator.journal, locator.journal_native)?;
                let mut journal = SessionJournal::open(&path).map_err(session_error)?;
                if let Some(request_id) = journal.snapshot().active_turn {
                    journal
                        .fail_turn(request_id, "operator_interrupted")
                        .map_err(session_error)?;
                }
                print_session(&journal)
            }
        },
        Command::Workspace { action } => {
            let data_root = data_root(cli.data_dir)?;
            control_cli::run(action, &data_root)
        }
        Command::File { action } => file_cli::run(action, &data_root(cli.data_dir)?),
        Command::Chat(args) => chat(cli.data_dir, args),
        Command::Run(args) => run_cli::run(args, &data_root(cli.data_dir)?),
        Command::Runs { action } => run_cli::runs(action, &data_root(cli.data_dir)?),
        Command::Packages { action } => packages_cli::run(action, &data_root(cli.data_dir)?),
        Command::Approvals { action } => approvals_cli::run(action, &data_root(cli.data_dir)?),
    }
}

/// Validate the provider before opening storage, then attach recovery information.
fn chat(data_dir: Option<PathBuf>, mut args: ChatArgs) -> Result<(), CliError> {
    let token = read_lm_token()?;
    let client = LocalModelClient::new(
        LocalModelConfig {
            base_url: args.endpoint.clone(),
            model: args.model.clone(),
            context_tokens: args.context_tokens,
            max_output_tokens: args.max_output_tokens,
        },
        // CLI retains one copy for text redaction; the client owns its header.
        token.clone(),
    )
    .map_err(provider_error)?;
    args.title = redact_known_secret(&args.title, token.as_deref());
    args.message = redact_known_secret(&args.message, token.as_deref());
    args.instructions = redact_known_secret(&args.instructions, token.as_deref());
    let path = if args.journal.is_some() || args.journal_native.is_some() {
        Some(resolve_journal(
            args.journal.take(),
            args.journal_native.take(),
        )?)
    } else {
        None
    };
    let data_root = data_root(data_dir)?;
    let mut journal = match path {
        Some(path) => SessionJournal::open(&path).map_err(session_error)?,
        None => {
            let workspace = args.workspace.as_deref().ok_or_else(|| {
                CliError::new("configuration", "A workspace or journal is required")
            })?;
            create_session(&data_root, workspace, &args.title)?
        }
    };
    let locator = journal_locator(&journal)?;
    chat_turn(&mut journal, &data_root, &client, args, token.as_deref())
        .map_err(|error| error.with_session(locator))
}

/// Preflight deterministic failures before starting; preserve uncertain I/O state.
fn chat_turn(
    journal: &mut SessionJournal,
    data_root: &std::path::Path,
    client: &LocalModelClient,
    args: ChatArgs,
    token: Option<&str>,
) -> Result<(), CliError> {
    if journal.snapshot().archived {
        return Err(CliError::new(
            "session",
            "Restore the archived session before chatting",
        ));
    }
    if journal.snapshot().active_turn.is_some() {
        return Err(CliError::new(
            "session",
            "The previous turn is incomplete; inspect it, then use session interrupt to mark it failed without retrying",
        ));
    }
    let manifest = RequestManifest {
        base_prompt: BASE_PROMPT.to_owned(),
        prompt_version: BASE_PROMPT_VERSION.to_owned(),
        prompt_sha256: prompt::base_prompt_sha256(),
        custom_instructions: args.instructions.clone(),
        model: args.model.clone(),
        base_url: args.endpoint.clone(),
        context_tokens: args.context_tokens,
        max_output_tokens: args.max_output_tokens,
        tool_schema_version: None,
        tools: None,
    };
    // Live settings decide whether this turn offers the workspace tools.
    let reason = match model_tools::select(data_root, journal)? {
        model_tools::ToolMode::Enabled { workspace_id } => {
            let output = model_tools::run(
                journal,
                data_root,
                workspace_id,
                client,
                token,
                model_tools::UserTurn {
                    message: &args.message,
                    instructions: &args.instructions,
                    manifest,
                },
            )?;
            return print_json(&JournalOutput {
                locator: journal_locator(journal)?,
                detail: output,
            });
        }
        model_tools::ToolMode::Disabled { reason } => reason,
    };
    // Text-only chat has no model file-read authority, so saved tool results
    // are withheld rather than sent again.
    let (mut history, withheld_tool_results) = replay_history(&journal.snapshot().messages, false);
    for message in &mut history {
        message.content = redact_known_secret(&message.content, token);
    }
    history.push(ConversationMessage {
        role: ConversationRole::User,
        content: args.message.clone(),
    });
    let messages = prompt::assemble(&args.instructions, &history)
        .map_err(|error| CliError::new("prompt", error.to_string()))?;
    journal
        .preflight_request(&args.message, &manifest)
        .map_err(session_error)?;
    let request_id = journal.start_turn().map_err(session_error)?;
    journal
        .record_message(MessageRole::User, &args.message)
        .map_err(session_error)?;
    journal
        .record_manifest(request_id, manifest)
        .map_err(session_error)?;
    let mut reply = match client.complete(&messages) {
        Ok(reply) => reply,
        Err(error) => {
            journal
                .fail_turn(request_id, "provider_error")
                .map_err(|_| {
                    CliError::new(
                        "persistence",
                        "Provider failed and failure could not be recorded",
                    )
                })?;
            return Err(provider_error(error));
        }
    };
    reply.content = redact_known_secret(&reply.content, token);
    // Keep every accepted reply resumable within the journal and prompt limits.
    // The wire response limit also covers protocol metadata, so it is larger.
    if reply.content.len() > 16 * 1024 {
        journal
            .fail_turn(request_id, "response_too_large")
            .map_err(session_error)?;
        return Err(CliError::new(
            "provider",
            "Assistant text exceeds the 16 KiB session message limit",
        ));
    }
    if let Err(error) = journal.record_message(MessageRole::Assistant, &reply.content) {
        if matches!(
            error,
            SessionError::LimitReached | SessionError::InvalidValue(_)
        ) {
            journal
                .fail_turn(request_id, "response_too_large")
                .map_err(session_error)?;
        }
        // Failed writes/syncs can have changed storage. Do not overwrite that
        // uncertain state with a fabricated terminal event or an automatic retry.
        return Err(session_error(error));
    }
    journal.complete_turn(request_id).map_err(session_error)?;
    print_json(&JournalOutput {
        locator: journal_locator(journal)?,
        detail: json!({
            "reply": reply,
            "model_tools": {
                "mode": "disabled",
                "reason": reason,
                "withheld_tool_results": withheld_tool_results,
            },
        }),
    })
}

/// Create a session with current registry metadata when a trusted root is configured.
///
/// An absent or valid unconfigured store preserves legacy session creation. Corrupt
/// control state or an invalid workspace under a configured root fails closed.
fn create_session(
    data_root: &std::path::Path,
    workspace: &std::path::Path,
    title: &str,
) -> Result<SessionJournal, CliError> {
    let registration_id = match ControlStore::open_existing(data_root)
        .map_err(|error| CliError::new("control", error.to_string()))?
    {
        Some(mut store) if store.has_development_root() => {
            let registration = store
                .register_workspace(workspace, store.revision())
                .map_err(|error| CliError::new("control", error.to_string()))?;
            let id = registration.id();
            drop(store);
            Some(id)
        }
        Some(store) => {
            drop(store);
            None
        }
        None => None,
    };

    let journal = match registration_id {
        Some(id) => SessionJournal::create_with_workspace_id(data_root, workspace, title, id),
        None => SessionJournal::create(data_root, workspace, title),
    }
    .map_err(session_error)?;
    Ok(journal)
}

/// Choose trusted application storage; repository content cannot set this value.
fn data_root(override_path: Option<PathBuf>) -> Result<PathBuf, CliError> {
    if let Some(path) = override_path {
        return Ok(path);
    }
    ProjectDirs::from("org", "OMG-Software", "OMGCode")
        .map(|directories| directories.data_local_dir().to_owned())
        .ok_or_else(|| CliError::new("configuration", "No per-user application data directory"))
}

fn print_session(journal: &SessionJournal) -> Result<(), CliError> {
    print_json(&JournalOutput {
        locator: journal_locator(journal)?,
        detail: json!({"session":journal.snapshot()}),
    })
}

/// Separate human display from a locator that preserves every native path unit.
fn journal_locator(journal: &SessionJournal) -> Result<JournalLocator, CliError> {
    Ok(JournalLocator {
        session_id: journal.snapshot().session_id,
        journal: journal.path().to_str().map(str::to_owned),
        journal_display: journal.path().to_string_lossy().into_owned(),
        journal_native: NativePath::from_path(journal.path()).map_err(session_error)?,
    })
}

/// Decode only the current OS's bounded native locator; never authorize its path.
fn resolve_journal(path: Option<PathBuf>, native: Option<String>) -> Result<PathBuf, CliError> {
    match (path, native) {
        (Some(path), None) => Ok(path),
        (None, Some(encoded)) if encoded.len() <= 256 * 1024 => {
            let native: NativePath = serde_json::from_str(&encoded)
                .map_err(|_| CliError::new("configuration", "Invalid native journal locator"))?;
            native.to_path_buf().map_err(session_error)
        }
        _ => Err(CliError::new(
            "configuration",
            "Supply one journal path or native locator",
        )),
    }
}

/// Read only the trusted CLI's credential input, without including it in errors.
/// Session creation and chat use this same source for known-token redaction.
fn read_lm_token() -> Result<Option<String>, CliError> {
    match env::var("OMGCODE_LM_TOKEN") {
        Ok(value) if !value.is_empty() => Ok(Some(value)),
        Ok(_) | Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(CliError::new(
            "configuration",
            "Server token must be Unicode",
        )),
    }
}

/// Remove the configured credential from untrusted text before retention or egress.
/// If replacement would itself contain the credential, discard that text instead.
fn redact_known_secret(text: &str, token: Option<&str>) -> String {
    match token.filter(|token| !token.is_empty()) {
        Some(token) => {
            let result = text.replace(token, "[REDACTED]");
            if result.contains(token) {
                String::new()
            } else {
                result
            }
        }
        None => text.to_owned(),
    }
}

/// Serialize untrusted strings as escaped JSON and propagate broken output errors.
fn print_json(value: &impl Serialize) -> Result<(), CliError> {
    let mut output = io::stdout().lock();
    serde_json::to_writer(&mut output, value)
        .map_err(|_| CliError::new("output", "Could not write JSON output"))?;
    output
        .write_all(b"\n")
        .map_err(|_| CliError::new("output", "Could not finish JSON output"))
}

fn session_error(error: omgcode_core::session::SessionError) -> CliError {
    CliError::new("session", error.to_string())
}

fn provider_error(error: omgcode_core::provider::ProviderError) -> CliError {
    CliError::new("provider", error.to_string())
}
