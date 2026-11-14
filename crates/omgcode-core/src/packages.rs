//! Trusted allowlisted package fetch.
//!
//! `packages fetch` is an operator command, never a model tool. It contacts
//! only an origin declared in a recorded package-network profile, refuses
//! redirects, and writes one bounded response into a private per-workspace
//! cache. The confined run still has no network access; exposing the cache to
//! it is a separate increment.
//!
//! The request URL text is not recorded. The control journal records the
//! intent and outcome with a URL digest, the profile, the cache name, byte
//! count, and content digest. A failed or abandoned fetch leaves its start
//! record; no automatic retry follows. See `docs/execution.md` and ADR-0035.

use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use reqwest::Url;
use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::control::{
    ControlError, ControlStore, PackageFetchFailure, PackageFetchFinish, PackageFetchOutcome,
    PackageFetchStart, PackageProfile, valid_output_name,
};
use crate::policy::WorkspaceId;
use crate::session::NativePath;
use crate::storage;

/// Default greatest response body accepted.
pub const DEFAULT_MAX_FETCH_BYTES: u64 = 64 * 1024 * 1024;
/// Largest accepted response body.
pub const MAX_FETCH_BYTES: u64 = 1024 * 1024 * 1024;
/// Default request deadline.
pub const DEFAULT_FETCH_TIMEOUT: Duration = Duration::from_secs(60);
/// Longest accepted request deadline.
pub const MAX_FETCH_TIMEOUT: Duration = Duration::from_secs(600);
/// Longest request URL accepted.
pub const MAX_FETCH_URL_BYTES: usize = 4096;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROL_WAIT: Duration = Duration::from_secs(10);
const CONTROL_POLL: Duration = Duration::from_millis(20);
const READ_CHUNK: usize = 64 * 1024;

/// One requested package fetch. Every field is operator input, not a grant.
#[derive(Clone, Debug)]
pub struct FetchRequest {
    /// The registered workspace whose private cache receives the file.
    pub workspace_id: WorkspaceId,
    /// The recorded package-network profile name.
    pub profile: String,
    /// The absolute registry URL.
    pub url: String,
    /// The cache file name.
    pub name: String,
    /// Greatest response body accepted.
    pub max_bytes: u64,
    /// Request deadline.
    pub timeout: Duration,
}

/// The receipt for one completed fetch. It never contains the URL text.
#[derive(Clone, Debug, Serialize)]
pub struct FetchReport {
    /// Durable audit identifier.
    pub fetch_id: Uuid,
    /// The selected profile.
    pub profile_id: Uuid,
    pub profile_name: String,
    pub profile_revision: u64,
    /// SHA-256 of the request URL.
    pub url_sha256: String,
    /// The private cache file.
    pub output: NativePath,
    /// Bytes written.
    pub bytes: u64,
    /// SHA-256 of the cached bytes.
    pub sha256: String,
    /// The final HTTP status.
    pub status: u16,
}

/// A controlled package-fetch failure.
#[derive(Debug, Error)]
pub enum FetchError {
    /// A check or bound refused the request; nothing was recorded.
    #[error("package fetch refused: {0}")]
    Refused(&'static str),
    /// The control journal could not be read or written.
    #[error("workspace control failed: {0}")]
    Control(#[from] ControlError),
    /// The request could not be sent or the connection failed.
    #[error("the registry request failed: {0}")]
    Transport(#[source] reqwest::Error),
    /// The response body could not be read.
    #[error("the response body could not be read: {0}")]
    Body(#[source] io::Error),
    /// The registry returned a non-success status.
    #[error("the registry returned HTTP status {0}")]
    Status(u16),
    /// The response body passed the size limit.
    #[error("the package is larger than the allowed {0} bytes")]
    TooLarge(u64),
    /// The cache directory or file could not be written or published.
    #[error("the package cache could not be written: {0}")]
    Cache(#[source] io::Error),
    /// The cache file already exists; fetch it under a new name.
    #[error("the cache file {0} already exists")]
    AlreadyCached(String),
    /// The file was published, but its outcome could not be recorded.
    #[error("the package was fetched, but its outcome could not be recorded: {source}")]
    OutcomeNotRecorded {
        report: Box<FetchReport>,
        #[source]
        source: ControlError,
    },
    /// The fetch failed, and recording the failure also failed.
    #[error("{failure}; its outcome could not be recorded: {audit}")]
    FailureAndAudit {
        #[source]
        failure: Box<FetchError>,
        audit: ControlError,
    },
}

struct FailedFetch {
    error: FetchError,
    reason: PackageFetchFailure,
    status: Option<u16>,
}

/// Fetch one allowed URL into the workspace's private cache.
///
/// The request must use a recorded package profile, the workspace must enable
/// `task_network`, the URL must match one declared origin and path prefix, and
/// no redirect is followed. A successful response is bounded, hashed, written
/// to a private file, synced, and published under a new name.
///
/// # Errors
///
/// Returns a typed failure. A request that passed the checks is recorded as
/// started before the network call; a later failure is recorded as a failed
/// outcome unless the journal itself is unavailable.
pub fn fetch(data_root: &Path, request: &FetchRequest) -> Result<FetchReport, FetchError> {
    validate_request(request)?;
    let control = open_control(data_root)?;
    let registration = control
        .workspace(request.workspace_id)
        .ok_or(FetchError::Refused("the workspace is not registered"))?;
    if !registration.permissions().task_network {
        return Err(FetchError::Refused(
            "the workspace task_network preference is disabled",
        ));
    }
    let status = control
        .package_profile(&request.profile)
        .ok_or(FetchError::Refused(
            "no recorded package-network profile has this name",
        ))?;
    let profile = status.profile();
    let profile_revision = status.revision();
    let url =
        Url::parse(&request.url).map_err(|_| FetchError::Refused("the URL must be absolute"))?;
    matching_origin(profile, &url)?;
    let cache = match control.package_cache_root(request.workspace_id) {
        Some(root) => {
            if !root.is_current().unwrap_or(false) {
                return Err(FetchError::Refused(
                    "the package cache root is stale; set it again",
                ));
            }
            root.live_path()
                .map_err(|_| FetchError::Refused("the package cache root is unavailable"))?
        }
        None => data_root
            .join("package-cache")
            .join(request.workspace_id.to_string()),
    };
    storage::create_private_directories(&cache)
        .map_err(|error| FetchError::Cache(io::Error::other(error.to_string())))?;
    let final_path = cache.join(&request.name);
    match fs::symlink_metadata(&final_path) {
        Ok(_) => return Err(FetchError::AlreadyCached(request.name.clone())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(FetchError::Cache(error)),
    }

    let start = PackageFetchStart {
        fetch_id: Uuid::new_v4(),
        workspace_id: request.workspace_id,
        profile_id: profile.id(),
        profile_name: profile.name().to_owned(),
        profile_revision,
        url_sha256: hex(&Sha256::digest(request.url.as_bytes())),
        output_name: request.name.clone(),
        max_bytes: request.max_bytes,
        timeout_ms: millis(request.timeout),
    };
    let mut control = control;
    control.record_package_fetch_started(start.clone())?;
    drop(control);

    let fetch_id = start.fetch_id;
    match perform(&start, &url, &cache, &final_path) {
        Ok((report, http_status)) => {
            let finish = PackageFetchFinish {
                fetch_id,
                outcome: PackageFetchOutcome::Fetched,
                status: Some(http_status),
                bytes: report.bytes,
                sha256: Some(report.sha256.clone()),
            };
            let recorded = open_control(data_root)
                .and_then(|mut store| store.record_package_fetch_finished(finish));
            if let Err(source) = recorded {
                return Err(FetchError::OutcomeNotRecorded {
                    report: Box::new(report),
                    source,
                });
            }
            Ok(report)
        }
        Err(failed) => {
            let finish = PackageFetchFinish {
                fetch_id,
                outcome: PackageFetchOutcome::Failed {
                    reason: failed.reason,
                },
                status: failed.status,
                bytes: 0,
                sha256: None,
            };
            let recorded = open_control(data_root)
                .and_then(|mut store| store.record_package_fetch_finished(finish));
            if let Err(audit) = recorded {
                return Err(FetchError::FailureAndAudit {
                    failure: Box::new(failed.error),
                    audit,
                });
            }
            Err(failed.error)
        }
    }
}

fn validate_request(request: &FetchRequest) -> Result<(), FetchError> {
    if request.max_bytes == 0 || request.max_bytes > MAX_FETCH_BYTES {
        return Err(FetchError::Refused(
            "the byte limit must be between 1 and 1073741824",
        ));
    }
    if request.timeout < Duration::from_secs(1) || request.timeout > MAX_FETCH_TIMEOUT {
        return Err(FetchError::Refused(
            "the timeout must be between 1 and 600 seconds",
        ));
    }
    if request.url.len() > MAX_FETCH_URL_BYTES {
        return Err(FetchError::Refused("the URL is too long"));
    }
    if !valid_output_name(&request.name) {
        return Err(FetchError::Refused("the cache name is invalid"));
    }
    Ok(())
}

/// Require the URL to match a declared origin and path prefix, with no
/// credentials, query, or fragment.
fn matching_origin(profile: &PackageProfile, url: &Url) -> Result<(), FetchError> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(FetchError::Refused("the URL must not contain credentials"));
    }
    if url.query().is_some() {
        return Err(FetchError::Refused("the URL must not contain a query"));
    }
    if url.fragment().is_some() {
        return Err(FetchError::Refused("the URL must not contain a fragment"));
    }
    let host = url
        .host_str()
        .ok_or(FetchError::Refused("the URL needs a host"))?;
    let port = url
        .port_or_known_default()
        .ok_or(FetchError::Refused("the URL needs a port"))?;
    let path = url.path();
    let matched = profile.origins().iter().any(|origin| {
        origin.scheme().as_str() == url.scheme()
            && origin.host().eq_ignore_ascii_case(host)
            && origin.port() == port
            && path.starts_with(origin.path_prefix())
    });
    if !matched {
        return Err(FetchError::Refused(
            "the URL is not under a declared registry origin and path",
        ));
    }
    Ok(())
}

fn perform(
    start: &PackageFetchStart,
    url: &Url,
    cache: &Path,
    final_path: &Path,
) -> Result<(FetchReport, u16), FailedFetch> {
    let timeout = Duration::from_millis(start.timeout_ms);
    let client = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(CONNECT_TIMEOUT.min(timeout))
        .timeout(timeout)
        .build()
        .map_err(|error| FailedFetch {
            error: FetchError::Transport(error),
            reason: PackageFetchFailure::Transport,
            status: None,
        })?;
    let mut response = client
        .get(url.as_str())
        .send()
        .map_err(|error| FailedFetch {
            error: FetchError::Transport(error),
            reason: PackageFetchFailure::Transport,
            status: None,
        })?;
    let http_status = response.status().as_u16();
    if !response.status().is_success() {
        return Err(FailedFetch {
            error: FetchError::Status(http_status),
            reason: PackageFetchFailure::Status,
            status: Some(http_status),
        });
    }

    let temporary = cache.join(format!(".{}.partial-{}", start.output_name, start.fetch_id));
    let mut file = open_cache_file(&temporary).map_err(|error| FailedFetch {
        error: FetchError::Cache(error),
        reason: PackageFetchFailure::Cache,
        status: Some(http_status),
    })?;
    let mut hasher = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = vec![0u8; READ_CHUNK];
    loop {
        let read = match response.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(FailedFetch {
                    error: FetchError::Body(error),
                    reason: PackageFetchFailure::Transport,
                    status: Some(http_status),
                });
            }
        };
        bytes = bytes.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if bytes > start.max_bytes {
            drop(file);
            let _ = fs::remove_file(&temporary);
            return Err(FailedFetch {
                error: FetchError::TooLarge(start.max_bytes),
                reason: PackageFetchFailure::TooLarge,
                status: Some(http_status),
            });
        }
        if let Err(error) = file.write_all(&buffer[..read]) {
            drop(file);
            let _ = fs::remove_file(&temporary);
            return Err(FailedFetch {
                error: FetchError::Cache(error),
                reason: PackageFetchFailure::Cache,
                status: Some(http_status),
            });
        }
        hasher.update(&buffer[..read]);
    }
    if let Err(error) = file.sync_all() {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(FailedFetch {
            error: FetchError::Cache(error),
            reason: PackageFetchFailure::Cache,
            status: Some(http_status),
        });
    }
    drop(file);

    // The cache is private to the user; a same-user writer can already change
    // it. Refuse a name that appeared after the pre-check, and publish with a
    // rename. No power-loss claim is made for this cache.
    match fs::symlink_metadata(final_path) {
        Ok(_) => {
            let _ = fs::remove_file(&temporary);
            return Err(FailedFetch {
                error: FetchError::AlreadyCached(start.output_name.clone()),
                reason: PackageFetchFailure::Cache,
                status: Some(http_status),
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(FailedFetch {
                error: FetchError::Cache(error),
                reason: PackageFetchFailure::Cache,
                status: Some(http_status),
            });
        }
    }
    if let Err(error) = fs::rename(&temporary, final_path) {
        let _ = fs::remove_file(&temporary);
        let already = fs::symlink_metadata(final_path).is_ok();
        let error = if already {
            FetchError::AlreadyCached(start.output_name.clone())
        } else {
            FetchError::Cache(error)
        };
        return Err(FailedFetch {
            error,
            reason: PackageFetchFailure::Cache,
            status: Some(http_status),
        });
    }

    Ok((
        FetchReport {
            fetch_id: start.fetch_id,
            profile_id: start.profile_id,
            profile_name: start.profile_name.clone(),
            profile_revision: start.profile_revision,
            url_sha256: start.url_sha256.clone(),
            output: NativePath::from_path(final_path).map_err(|error| FailedFetch {
                error: FetchError::Cache(io::Error::other(error.to_string())),
                reason: PackageFetchFailure::Cache,
                status: Some(http_status),
            })?,
            bytes,
            sha256: hex(&hasher.finalize()),
            status: http_status,
        },
        http_status,
    ))
}

fn open_cache_file(path: &Path) -> io::Result<std::fs::File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Open existing settings, waiting briefly for another command's lock.
fn open_control(data_root: &Path) -> Result<ControlStore, ControlError> {
    let until = Instant::now() + CONTROL_WAIT;
    loop {
        match ControlStore::open_existing(data_root) {
            Ok(Some(store)) => return Ok(store),
            Ok(None) => {
                return Err(ControlError::Io(io::Error::new(
                    io::ErrorKind::NotFound,
                    "workspace control is not initialized",
                )));
            }
            Err(ControlError::Busy) if Instant::now() < until => thread::sleep(CONTROL_POLL),
            Err(error) => return Err(error),
        }
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Writing to a `String` cannot fail.
        let _ = write!(&mut text, "{byte:02x}");
    }
    text
}

#[cfg(test)]
mod tests;
