//! Trusted operator package fetches.
#![forbid(unsafe_code)]

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use clap::{Args, Subcommand};
use omgcode_core::packages::{
    self, DEFAULT_FETCH_TIMEOUT, DEFAULT_MAX_FETCH_BYTES, FetchError, FetchRequest,
    MAX_FETCH_BYTES, MAX_FETCH_TIMEOUT,
};
use omgcode_core::policy::WorkspaceId;
use serde_json::json;

use crate::{CliError, print_json};

/// Fetch one package from a declared registry into a private cache.
#[derive(Subcommand)]
pub(crate) enum PackagesCommand {
    Fetch(FetchArgs),
}

#[derive(Args)]
pub(crate) struct FetchArgs {
    /// The registered workspace whose private cache receives the file.
    workspace: String,
    /// The recorded package-network profile to use.
    #[arg(long)]
    profile: String,
    /// The absolute registry URL. It must match a declared origin and path.
    #[arg(long)]
    url: String,
    /// The cache file name. It must be a new single file name.
    #[arg(long)]
    name: String,
    /// Greatest response body accepted (1 to 1073741824 bytes).
    #[arg(long, default_value_t = DEFAULT_MAX_FETCH_BYTES)]
    max_bytes: u64,
    /// Request deadline (1 to 600 seconds).
    #[arg(long, default_value_t = DEFAULT_FETCH_TIMEOUT.as_secs())]
    timeout_secs: u64,
}

/// Run one trusted package command.
pub(crate) fn run(action: PackagesCommand, data_root: &Path) -> Result<(), CliError> {
    match action {
        PackagesCommand::Fetch(args) => fetch(args, data_root),
    }
}

fn fetch(args: FetchArgs, data_root: &Path) -> Result<(), CliError> {
    let workspace_id = WorkspaceId::from_str(&args.workspace)
        .map_err(|_| CliError::new("configuration", "Invalid workspace identifier"))?;
    if args.max_bytes == 0 || args.max_bytes > MAX_FETCH_BYTES {
        return Err(CliError::new(
            "configuration",
            "Byte limit must be 1 to 1073741824",
        ));
    }
    if args.timeout_secs == 0 || Duration::from_secs(args.timeout_secs) > MAX_FETCH_TIMEOUT {
        return Err(CliError::new(
            "configuration",
            "Timeout must be 1 to 600 seconds",
        ));
    }
    let request = FetchRequest {
        workspace_id,
        profile: args.profile,
        url: args.url,
        name: args.name,
        max_bytes: args.max_bytes,
        timeout: Duration::from_secs(args.timeout_secs),
    };
    match packages::fetch(data_root, &request) {
        Ok(report) => print_json(&json!({ "fetch": report })),
        Err(FetchError::OutcomeNotRecorded { report, source }) => {
            print_json(&json!({ "fetch": report }))?;
            Err(CliError::new(
                "packages",
                format!("The package was fetched, but its outcome could not be recorded: {source}"),
            ))
        }
        Err(error) => Err(CliError::new("packages", error.to_string())),
    }
}
