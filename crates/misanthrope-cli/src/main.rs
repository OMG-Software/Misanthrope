//! Developer CLI for the first Misanthrope core prototype.
#![forbid(unsafe_code)]

use clap::{Args, Parser, Subcommand};
use directories::ProjectDirs;
use misanthrope_core::prompt::{
    self, BASE_PROMPT, BASE_PROMPT_VERSION, ConversationMessage, ConversationRole,
};
use misanthrope_core::provider::{LocalModelClient, LocalModelConfig};
use misanthrope_core::session::{MessageRole, RequestManifest, SessionJournal};
use serde::Serialize;
use serde_json::json;
use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "misanthrope",
    version,
    about = "Misanthrope local core prototype"
)]
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
    /// Send one turn to an explicitly selected local LM Studio model.
    Chat(ChatArgs),
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
        journal: PathBuf,
    },
    Archive {
        journal: PathBuf,
    },
    Restore {
        journal: PathBuf,
    },
    /// Mark an abandoned turn failed without retrying its model request.
    Interrupt {
        journal: PathBuf,
    },
}

#[derive(Args)]
struct ChatArgs {
    #[arg(long, required_unless_present = "journal", conflicts_with = "journal")]
    workspace: Option<PathBuf>,
    /// Resume this explicit journal. Historical grants never authorize tools.
    #[arg(long)]
    journal: Option<PathBuf>,
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

struct CliError {
    code: &'static str,
    message: String,
}

impl CliError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // A closed error stream cannot be repaired by recursively printing.
            if serde_json::to_writer(
                io::stderr().lock(),
                &json!({"error": {"code": error.code, "message": error.message}}),
            )
            .is_err()
            {
                return ExitCode::FAILURE;
            }
            ExitCode::FAILURE
        }
    }
}

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
            "workspace_file_tools": false,
            "process_execution": false,
            "mcp": false,
            "memories": false,
            "sandbox_backend": "unavailable",
            "tool_policy": "reject all model-requested tools",
        })),
        Command::Session { action } => match action {
            SessionCommand::New { workspace, title } => {
                let data_root = data_root(cli.data_dir)?;
                let journal = SessionJournal::create(&data_root, &workspace, &title)
                    .map_err(session_error)?;
                print_session(&journal)
            }
            SessionCommand::Inspect { journal } => {
                print_session(&SessionJournal::open(&journal).map_err(session_error)?)
            }
            SessionCommand::Archive { journal } => {
                let mut journal = SessionJournal::open(&journal).map_err(session_error)?;
                journal.archive().map_err(session_error)?;
                print_session(&journal)
            }
            SessionCommand::Restore { journal } => {
                let mut journal = SessionJournal::open(&journal).map_err(session_error)?;
                journal.restore().map_err(session_error)?;
                print_session(&journal)
            }
            SessionCommand::Interrupt { journal } => {
                let mut journal = SessionJournal::open(&journal).map_err(session_error)?;
                if let Some(request_id) = journal.snapshot().active_turn {
                    journal
                        .fail_turn(request_id, "operator_interrupted")
                        .map_err(session_error)?;
                }
                print_session(&journal)
            }
        },
        Command::Chat(args) => chat(cli.data_dir, args),
    }
}

fn chat(data_dir: Option<PathBuf>, args: ChatArgs) -> Result<(), CliError> {
    let token = match env::var("MISANTHROPE_LM_TOKEN") {
        Ok(value) if !value.is_empty() => Some(value),
        Ok(_) | Err(env::VarError::NotPresent) => None,
        Err(env::VarError::NotUnicode(_)) => {
            return Err(CliError::new(
                "configuration",
                "Server token must be Unicode",
            ));
        }
    };
    let client = LocalModelClient::new(
        LocalModelConfig {
            base_url: args.endpoint.clone(),
            model: args.model.clone(),
            context_tokens: args.context_tokens,
            max_output_tokens: args.max_output_tokens,
        },
        token,
    )
    .map_err(provider_error)?;
    let mut journal = match &args.journal {
        Some(path) => SessionJournal::open(path).map_err(session_error)?,
        None => {
            let workspace = args.workspace.as_deref().ok_or_else(|| {
                CliError::new("configuration", "A workspace or journal is required")
            })?;
            SessionJournal::create(&data_root(data_dir)?, workspace, &args.title)
                .map_err(session_error)?
        }
    };
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
    let mut history: Vec<ConversationMessage> = journal
        .snapshot()
        .messages
        .iter()
        .map(|message| ConversationMessage {
            role: match message.role {
                MessageRole::User => ConversationRole::User,
                MessageRole::Assistant => ConversationRole::Assistant,
            },
            content: message.text.clone(),
        })
        .collect();
    history.push(ConversationMessage {
        role: ConversationRole::User,
        content: args.message.clone(),
    });
    let messages = prompt::assemble(&args.instructions, &history)
        .map_err(|error| CliError::new("prompt", error.to_string()))?;
    journal
        .record_message(MessageRole::User, &args.message)
        .map_err(session_error)?;
    let request_id = journal.start_turn().map_err(session_error)?;
    journal
        .record_manifest(
            request_id,
            RequestManifest {
                base_prompt: BASE_PROMPT.to_owned(),
                prompt_version: BASE_PROMPT_VERSION.to_owned(),
                prompt_sha256: prompt::base_prompt_sha256(),
                custom_instructions: args.instructions,
                model: args.model,
                base_url: args.endpoint,
                context_tokens: args.context_tokens,
                max_output_tokens: args.max_output_tokens,
            },
        )
        .map_err(session_error)?;
    let reply = match client.complete(&messages) {
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
    journal
        .record_message(MessageRole::Assistant, &reply.content)
        .map_err(session_error)?;
    journal.complete_turn(request_id).map_err(session_error)?;
    print_json(&json!({
        "session_id": journal.snapshot().session_id,
        "journal": display_path(journal.path()),
        "reply": reply,
    }))
}

fn data_root(override_path: Option<PathBuf>) -> Result<PathBuf, CliError> {
    if let Some(path) = override_path {
        return Ok(path);
    }
    ProjectDirs::from("org", "OMG-Software", "Misanthrope")
        .map(|directories| directories.data_local_dir().to_owned())
        .ok_or_else(|| CliError::new("configuration", "No per-user application data directory"))
}

fn print_session(journal: &SessionJournal) -> Result<(), CliError> {
    print_json(&json!({
        "journal": display_path(journal.path()),
        "session": journal.snapshot(),
    }))
}

fn display_path(path: &Path) -> String {
    // Display only. Reopening always uses the original native Path supplied by the caller.
    path.to_string_lossy().into_owned()
}

fn print_json(value: &impl Serialize) -> Result<(), CliError> {
    let mut output = io::stdout().lock();
    serde_json::to_writer(&mut output, value)
        .map_err(|_| CliError::new("output", "Could not write JSON output"))?;
    output
        .write_all(b"\n")
        .map_err(|_| CliError::new("output", "Could not finish JSON output"))
}

fn session_error(error: misanthrope_core::session::SessionError) -> CliError {
    CliError::new("session", error.to_string())
}

fn provider_error(error: misanthrope_core::provider::ProviderError) -> CliError {
    CliError::new("provider", error.to_string())
}
