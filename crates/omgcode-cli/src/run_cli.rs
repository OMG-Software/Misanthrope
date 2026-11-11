//! Trusted operator command runs and their journal.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use clap::{Args, Subcommand};
use omgcode_core::policy::WorkspaceId;
use omgcode_core::run::{
    self, CancelSignal, MAX_CAPTURE_BYTES, MAX_DEADLINE, ProcessLimits, RunError, RunRequest,
    cancel_on_interrupt,
};
use serde_json::json;
use uuid::Uuid;

use crate::{CliError, print_json};

/// Run one program in a workspace under native confinement.
#[derive(Args)]
pub(crate) struct RunArgs {
    /// The workspace identifier. Its `execute` preference must be enabled.
    workspace: String,
    /// The recorded toolchain profile to use.
    #[arg(long)]
    profile: String,
    /// Workspace-relative working directory.
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Seconds before the whole process tree is terminated (1 to 3600).
    #[arg(long, default_value_t = 600)]
    timeout_secs: u64,
    /// Bytes of each output stream to return (at most 16 MiB).
    #[arg(long, default_value_t = 1024 * 1024)]
    max_output_bytes: usize,
    /// Allow host Unix-domain sockets, such as the Docker daemon. Needs the
    /// workspace `host_services` preference. Refused on Windows.
    #[arg(long)]
    host_services: bool,
    /// The program, then its arguments. Put `--` before them.
    #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<OsString>,
}

/// Inspect, acknowledge, or archive run records.
#[derive(Subcommand)]
pub(crate) enum RunsCommand {
    /// Show recent run records and pending runs.
    Audit {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Inspect an archived journal instead of the active one.
        #[arg(long)]
        archive: Option<PathBuf>,
    },
    /// Clear a pending run after inspecting it. Retries nothing.
    Acknowledge { run: Uuid },
    /// Move a journal with no pending runs into an existing directory.
    Archive {
        #[arg(long)]
        archive_dir: PathBuf,
    },
}

/// Check, record, and run one program, then print the recorded result.
///
/// The command exits successfully whenever the run was supervised and its
/// outcome recorded, whatever the program's own exit code; read
/// `run.finish.result` for that.
pub(crate) fn run(args: RunArgs, data_root: &Path) -> Result<(), CliError> {
    let workspace_id = WorkspaceId::from_str(&args.workspace)
        .map_err(|_| CliError::new("configuration", "Invalid workspace identifier"))?;
    let deadline = Duration::from_secs(args.timeout_secs);
    if deadline.is_zero() || deadline > MAX_DEADLINE {
        return Err(CliError::new(
            "configuration",
            "Timeout must be 1 to 3600 seconds",
        ));
    }
    if args.max_output_bytes > MAX_CAPTURE_BYTES {
        return Err(CliError::new(
            "configuration",
            "Output limit must be at most 16 MiB",
        ));
    }
    let mut command = args.command.into_iter();
    let program = command
        .next()
        .ok_or_else(|| CliError::new("configuration", "A program is required"))?;
    let request = RunRequest {
        workspace_id,
        profile: args.profile,
        program: PathBuf::from(program),
        arguments: command.collect(),
        working_directory: args.cwd,
        host_services: args.host_services,
        limits: ProcessLimits {
            deadline,
            max_stdout_bytes: args.max_output_bytes,
            max_stderr_bytes: args.max_output_bytes,
            ..ProcessLimits::default()
        },
    };
    let cancel = CancelSignal::new();
    cancel_on_interrupt(&cancel).map_err(|error| {
        CliError::new(
            "run",
            format!("Interrupt handling could not be installed: {error}"),
        )
    })?;
    match run::run(data_root, &request, &cancel) {
        Ok(report) => print_json(&json!({ "run": report })),
        Err(RunError::OutcomeNotRecorded { report, source }) => {
            // Keep the output even though the journal still shows the run
            // as pending.
            print_json(&json!({ "run": report }))?;
            Err(CliError::new(
                "run",
                format!(
                    "Run {} ended, but its outcome could not be recorded: {source}",
                    report.start.run_id
                ),
            ))
        }
        Err(error) => Err(CliError::new("run", error.to_string())),
    }
}

/// Inspect, acknowledge, or archive run records.
pub(crate) fn runs(action: RunsCommand, data_root: &Path) -> Result<(), CliError> {
    match action {
        RunsCommand::Audit { limit, archive } => {
            let view = match archive {
                Some(path) => run::audit_archive(data_root, &path, limit),
                None => run::audit(data_root, limit),
            }
            .map_err(run_error)?;
            print_json(&view)
        }
        RunsCommand::Acknowledge { run } => {
            let scratch_removed = run::acknowledge(data_root, run).map_err(run_error)?;
            print_json(&json!({ "acknowledged": run, "scratch_removed": scratch_removed }))
        }
        RunsCommand::Archive { archive_dir } => {
            print_json(&run::archive(data_root, &archive_dir).map_err(run_error)?)
        }
    }
}

fn run_error(error: RunError) -> CliError {
    CliError::new("run", error.to_string())
}
