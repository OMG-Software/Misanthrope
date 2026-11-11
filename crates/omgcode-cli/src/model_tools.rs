//! Chat with the persisted model read/list tool preference.
//!
//! The session journal only selects which registered workspace to consult.
//! Authority comes from the live settings preference, rechecked by the core
//! runtime before every tool call, and each call stays inside that workspace.

use std::path::Path;
use std::time::Duration;

use omgcode_core::control::ControlStore;
use omgcode_core::policy::WorkspaceId;
use omgcode_core::prompt::ModelMessage;
use omgcode_core::provider::{LocalModelClient, LocalToolModel, ProviderError};
use omgcode_core::session::{RequestManifest, SessionJournal};
use omgcode_core::turn::{
    ModelError, ToolModel, ToolModelReply, ToolModelRequest, TurnError, TurnLimits, TurnReport,
    TurnRuntime,
};
use serde::Serialize;

use crate::{CliError, provider_error, redact_known_secret};

/// Whether this chat turn offers the model the workspace read/list tools.
#[derive(Debug)]
pub(crate) enum ToolMode {
    /// The selected workspace's live preference enables model tools.
    Enabled { workspace_id: WorkspaceId },
    /// Text-only chat, with the reason tools were not offered.
    Disabled { reason: &'static str },
}

/// Decide the tool mode from current settings without holding the settings lock.
///
/// An absent store, an unregistered workspace, or a disabled preference gives
/// text-only chat. Invalid settings fail closed. A version 1 journal cannot
/// record tool events, so it stays text-only.
pub(crate) fn select(data_root: &Path, journal: &SessionJournal) -> Result<ToolMode, CliError> {
    let Some(control) = ControlStore::open_existing(data_root)
        .map_err(|error| CliError::new("control", error.to_string()))?
    else {
        return Ok(ToolMode::Disabled {
            reason: "settings_absent",
        });
    };
    // The journal's UUID only selects a registration; the ID used afterwards
    // and its preference come from current settings.
    let (workspace_id, enabled) =
        match control.registered_workspace(journal.snapshot().workspace_id) {
            None => {
                return Ok(ToolMode::Disabled {
                    reason: "workspace_unregistered",
                });
            }
            Some(workspace) => (workspace.id(), workspace.permissions().model_tools),
        };
    // Release the settings lock before any model request.
    drop(control);
    if !enabled {
        return Ok(ToolMode::Disabled {
            reason: "preference_disabled",
        });
    }
    if journal.schema_version() != 2 {
        return Ok(ToolMode::Disabled {
            reason: "session_schema_v1",
        });
    }
    Ok(ToolMode::Enabled { workspace_id })
}

/// Summary of one completed tool-enabled turn.
#[derive(Serialize)]
pub(crate) struct ToolTurnOutput {
    pub(crate) reply: ToolTurnReply,
    pub(crate) model_tools: ToolTurnSummary,
}

/// Assistant text from a tool-enabled turn. It remains untrusted model output.
#[derive(Serialize)]
pub(crate) struct ToolTurnReply {
    pub(crate) content: String,
}

/// Bounded counters for a tool-enabled turn.
#[derive(Serialize)]
pub(crate) struct ToolTurnSummary {
    mode: &'static str,
    workspace_id: String,
    rounds: usize,
    tool_calls: usize,
    tool_bytes: u64,
}

/// The user's request for one tool-enabled turn.
pub(crate) struct UserTurn<'a> {
    pub(crate) message: &'a str,
    pub(crate) instructions: &'a str,
    pub(crate) manifest: RequestManifest,
}

/// Run one tool-enabled turn through the core runtime.
pub(crate) fn run(
    journal: &mut SessionJournal,
    data_root: &Path,
    workspace_id: WorkspaceId,
    client: &LocalModelClient,
    token: Option<&str>,
    turn: UserTurn<'_>,
) -> Result<ToolTurnOutput, CliError> {
    let mut model = RedactingModel {
        inner: LocalToolModel::new(client),
        token,
    };
    // Tool results are recorded before the next model request, so they need
    // their own redaction; the model wrapper sees them only afterwards.
    let redact_tool_text = |text: &str| redact_known_secret(text, token);
    let report: TurnReport = TurnRuntime::new(journal, data_root, workspace_id, chat_limits())
        .and_then(|runtime| {
            runtime.with_tool_text_filter(&redact_tool_text).run(
                &mut model,
                turn.message,
                turn.instructions,
                turn.manifest,
            )
        })
        .map_err(|error| turn_error(error, model.inner.last_error()))?;
    Ok(ToolTurnOutput {
        reply: ToolTurnReply {
            content: report.reply,
        },
        model_tools: ToolTurnSummary {
            mode: "enabled",
            workspace_id: workspace_id.to_string(),
            rounds: report.rounds,
            tool_calls: report.tool_calls,
            tool_bytes: report.tool_bytes,
        },
    })
}

/// Cooperative deadline for one tool-enabled chat turn.
///
/// Each local model request may take up to 30 seconds, and a turn makes at most
/// four requests, so the 30-second library default would stop ordinary turns.
const CHAT_TURN_DEADLINE: Duration = Duration::from_secs(120);

/// Runtime limits for tool-enabled chat: library maxima with a chat deadline.
fn chat_limits() -> TurnLimits {
    TurnLimits {
        deadline: CHAT_TURN_DEADLINE,
        ..TurnLimits::default()
    }
}

/// Remove a known local server token from text sent to and received from the model.
struct RedactingModel<'a> {
    inner: LocalToolModel<'a>,
    token: Option<&'a str>,
}

impl ToolModel for RedactingModel<'_> {
    fn complete(&mut self, request: ToolModelRequest<'_>) -> Result<ToolModelReply, ModelError> {
        let messages: Vec<ModelMessage> = request
            .messages
            .iter()
            .map(|message| ModelMessage {
                role: message.role,
                content: redact_known_secret(&message.content, self.token),
            })
            .collect();
        let mut reply = self.inner.complete(ToolModelRequest {
            messages: &messages,
            tools: request.tools,
        })?;
        reply.content = redact_known_secret(&reply.content, self.token);
        Ok(reply)
    }
}

/// Map a runtime failure to a stable CLI error without echoing model text.
fn turn_error(error: TurnError, provider: Option<ProviderError>) -> CliError {
    match error {
        TurnError::Model(_) => provider.map_or_else(
            || CliError::new("provider", "Local model request failed"),
            provider_error,
        ),
        TurnError::ToolDenied(_) => CliError::new(
            "permission",
            "A model tool call was denied by the current workspace preferences",
        ),
        TurnError::InvalidToolCall(error) => CliError::new(
            "tool_call",
            format!("The model returned an invalid tool call: {error}"),
        ),
        TurnError::Limit(limit) => {
            CliError::new("tool_limit", format!("Turn limit exceeded: {limit}"))
        }
        TurnError::Session(error) => crate::session_error(error),
        TurnError::Prompt(error) => CliError::new("prompt", error.to_string()),
        TurnError::Archived => {
            CliError::new("session", "Restore the archived session before chatting")
        }
        TurnError::ActiveTurn => CliError::new(
            "session",
            "The previous turn is incomplete; inspect it, then use session interrupt to mark it failed without retrying",
        ),
        other => CliError::new("turn", other.to_string()),
    }
}
