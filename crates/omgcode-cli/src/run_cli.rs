//! Trusted operator command runs and their journal.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use clap::{Args, Subcommand};
use omgcode_core::policy::WorkspaceId;
use omgcode_core::run::{
    self, CancelSignal, DEFAULT_TREE_CPU_TIME, DEFAULT_TREE_DISK_BYTES, DEFAULT_TREE_MEMORY_BYTES,
    DEFAULT_TREE_PROCESSES, MAX_CAPTURE_BYTES, MAX_DEADLINE, MAX_TREE_CPU_TIME,
    MAX_TREE_DISK_BYTES, MAX_TREE_MEMORY_BYTES, MAX_TREE_PROCESSES, MIN_TREE_DISK_BYTES,
    MIN_TREE_MEMORY_BYTES, ProcessLimits, RunError, RunRequest, TreeLimits, cancel_on_interrupt,
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
    /// Memory for the whole process tree, in MiB (16 to 1048576).
    #[arg(long, default_value_t = DEFAULT_TREE_MEMORY_BYTES / MIB)]
    max_memory_mib: u64,
    /// Processes alive at once in the tree (1 to 65536). Linux counts
    /// threads too.
    #[arg(long, default_value_t = DEFAULT_TREE_PROCESSES)]
    max_processes: u32,
    /// CPU seconds for the whole tree, summed over every process (1 to
    /// 604800).
    #[arg(long, default_value_t = DEFAULT_TREE_CPU_TIME.as_secs())]
    max_cpu_secs: u64,
    /// Bytes the run may add to its writable roots, in MiB (16 to 1048576).
    /// Allocated bytes and file lengths are bounded separately. Sampled once a
    /// second, not a filesystem quota. See ADR-0033.
    #[arg(long, default_value_t = DEFAULT_TREE_DISK_BYTES / MIB)]
    max_disk_mib: u64,
    /// Allow host Unix-domain sockets, such as the Docker daemon. Needs the
    /// workspace `host_services` preference. Refused on Windows.
    #[arg(long)]
    host_services: bool,
    /// Allow outbound IP networking. Needs the workspace `task_network`
    /// preference. On Linux the program can reach any address, including
    /// loopback services and the local network; on Windows, only the
    /// internet. See ADR-0032.
    #[arg(long)]
    network: bool,
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
    let tree = tree_limits(
        args.max_memory_mib,
        args.max_processes,
        args.max_cpu_secs,
        args.max_disk_mib,
    )?;
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
        network: args.network,
        limits: ProcessLimits {
            deadline,
            max_stdout_bytes: args.max_output_bytes,
            max_stderr_bytes: args.max_output_bytes,
            tree: Some(tree),
            ..ProcessLimits::default()
        },
    };
    let cancel = CancelSignal::new();
    let interrupts = cancel_on_interrupt(&cancel).map_err(|error| {
        CliError::new(
            "run",
            format!("Interrupt handling could not be installed: {error}"),
        )
    })?;
    let result = run::run(data_root, &request, &cancel);
    // The watcher is joined on every path; its failure cannot change the
    // recorded outcome, so it is not reported over the run's result.
    let _ = interrupts.stop();
    match result {
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

const MIB: u64 = 1024 * 1024;

/// Check the tree-limit flags against the runner's ranges.
fn tree_limits(
    memory_mib: u64,
    processes: u32,
    cpu_secs: u64,
    disk_mib: u64,
) -> Result<TreeLimits, CliError> {
    let memory = memory_mib
        .checked_mul(MIB)
        .filter(|bytes| (MIN_TREE_MEMORY_BYTES..=MAX_TREE_MEMORY_BYTES).contains(bytes))
        .ok_or_else(|| CliError::new("configuration", "Memory limit must be 16 to 1048576 MiB"))?;
    if processes == 0 || processes > MAX_TREE_PROCESSES {
        return Err(CliError::new(
            "configuration",
            "Process limit must be 1 to 65536",
        ));
    }
    let cpu = Duration::from_secs(cpu_secs);
    if cpu.is_zero() || cpu > MAX_TREE_CPU_TIME {
        return Err(CliError::new(
            "configuration",
            "CPU-time limit must be 1 to 604800 seconds",
        ));
    }
    let disk = disk_mib
        .checked_mul(MIB)
        .filter(|bytes| (MIN_TREE_DISK_BYTES..=MAX_TREE_DISK_BYTES).contains(bytes))
        .ok_or_else(|| CliError::new("configuration", "Disk limit must be 16 to 1048576 MiB"))?;
    Ok(TreeLimits {
        max_memory_bytes: memory,
        max_processes: processes,
        max_cpu_time: cpu,
        max_disk_bytes: Some(disk),
    })
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
