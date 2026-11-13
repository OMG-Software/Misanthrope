//! Filesystem and network confinement requested for one run.
//!
//! The rules are an allowlist: a confined program can reach only the listed
//! paths, creates IP sockets only when [`NetworkAccess::Internet`] is
//! selected, follows the selected Unix-domain socket mode, and passes the same
//! restrictions to every descendant. Linux applies them with Landlock and
//! seccomp. Windows runs the tree in an AppContainer and checks that each
//! listed path is already granted to it. See `docs/execution.md`, ADR-0028,
//! ADR-0029, and ADR-0032.

use std::path::{Path, PathBuf};

use super::{ProcessError, contains_nul};
use crate::container::ContainerName;

/// Largest number of paths, read-only and read-write together, in one confinement.
pub const MAX_CONFINEMENT_PATHS: usize = 64;

/// Filesystem allowlist for a confined run. Operations that Landlock handles,
/// such as opening, executing, listing, creating, renaming, truncating, and
/// removing, are denied outside the listed paths. Landlock does not restrict
/// metadata changes: a confined program can still change permissions,
/// ownership (within the user's rights), timestamps, and extended attributes
/// of any path it can name. See `docs/execution.md` for the full limits.
///
/// Each path must be absolute and name an existing directory, regular file,
/// or character device. On Linux no component of the path may be a symbolic
/// link, so a rule always names the object the caller checked. A directory
/// rule covers everything beneath it. Rules are additive: a read-only entry
/// inside a read-write directory is still writable.
///
/// On Windows the rules are not applied at start. Access comes from the
/// persistent grants made with [`crate::container::grant`]; each listed path
/// must already carry a grant of at least its listed level for
/// [`Confinement::container`], or nothing starts. Paths every application
/// package can read, such as the Windows directory, stay readable, and grants
/// that are not listed still apply.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Confinement {
    /// Paths whose contents may be read and executed, and whose directories
    /// may be listed, but not written, truncated, or changed beneath.
    /// Metadata changes are not prevented. Toolchain roots, system
    /// libraries, and the program itself belong here.
    pub read_only: Vec<PathBuf>,
    /// Paths that may also be written, created, renamed, and removed beneath.
    /// The workspace and a private scratch directory belong here.
    pub read_write: Vec<PathBuf>,
    /// Which Unix-domain sockets the tree may create. Defaults to pairs only.
    /// Windows refuses [`UnixSocketAccess::HostServices`].
    pub unix_sockets: UnixSocketAccess,
    /// Whether the tree may use IP networking. Defaults to none.
    pub network: NetworkAccess,
    /// The AppContainer identity for the run. Windows requires it; Linux
    /// ignores it.
    pub container: Option<ContainerName>,
}

/// Whether a confined tree may use IP networking.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NetworkAccess {
    /// No IP sockets on Linux; no network capability on Windows. This is the
    /// default.
    #[default]
    Denied,
    /// Outbound IP networking, which differs by platform:
    ///
    /// - **Linux:** `socket` may create `AF_INET` and `AF_INET6` sockets of
    ///   any type the user may create. Nothing limits the destination: the
    ///   tree can reach **loopback services, the local network, and cloud
    ///   metadata endpoints** as well as the internet. On Landlock ABI 4 and
    ///   later, an explicit TCP `bind` stays denied, but the tree can still
    ///   accept TCP connections: `listen` on an unbound socket takes a port
    ///   the kernel chooses, which Landlock does not check. UDP and older ABIs
    ///   are not limited.
    /// - **Windows:** the AppContainer gets the `internetClient` capability:
    ///   outbound connections to the internet and public networks. Private
    ///   networks, loopback, and inbound connections stay denied.
    ///
    /// Either way the tree can send any workspace data it can read to any
    /// host it can reach. Select this only on an explicit operator decision.
    Internet,
}

/// Which Unix-domain sockets a confined tree may create. IP follows
/// [`NetworkAccess`]; every other address family stays denied in both modes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UnixSocketAccess {
    /// Only `socketpair` of stream or sequenced-packet type, for IPC inside
    /// the tree. No socket can connect to a host service. This is the default.
    #[default]
    PairsOnly,
    /// Also `socket(AF_UNIX)` of any type, and creating socket files in
    /// read-write paths. The tree can then connect to **every pathname
    /// socket the user can reach**, including the Docker daemon, the D-Bus
    /// session bus, and SSH agents; Landlock cannot limit which ones.
    /// Connecting to the Docker daemon is usually equivalent to root on the
    /// host. Select this only on an explicit operator decision.
    HostServices,
}

impl Confinement {
    /// Check the shape of every rule before anything is opened or started.
    pub(super) fn validate(&self) -> Result<(), ProcessError> {
        let count = self.read_only.len().saturating_add(self.read_write.len());
        if count > MAX_CONFINEMENT_PATHS {
            return Err(ProcessError::InvalidSpec("too many confinement paths"));
        }
        for path in self.read_only.iter().chain(&self.read_write) {
            validate_path(path)?;
        }
        Ok(())
    }
}

fn validate_path(path: &Path) -> Result<(), ProcessError> {
    if !path.is_absolute() {
        return Err(ProcessError::InvalidSpec(
            "confinement paths must be absolute",
        ));
    }
    if contains_nul(path.as_os_str()) {
        return Err(ProcessError::InvalidSpec(
            "confinement paths must not contain NUL",
        ));
    }
    Ok(())
}

/// Host files that name resolution and TLS read on Linux. With network access
/// a confined program can open sockets, but it can read only listed paths, so
/// without these it can neither resolve a name nor verify a certificate.
///
/// Certificate entries name public trust stores and OpenSSL configuration
/// only, never `/etc/ssl` or `/etc/pki` themselves, because those hold the
/// host's private keys beneath them.
#[cfg(target_os = "linux")]
const NETWORK_SUPPORT_PATHS: [&str; 17] = [
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/nsswitch.conf",
    "/etc/gai.conf",
    "/etc/host.conf",
    "/etc/services",
    "/etc/protocols",
    "/etc/ssl/certs",
    "/etc/ssl/cert.pem",
    "/etc/ssl/openssl.cnf",
    "/etc/pki/tls/certs",
    "/etc/pki/tls/cert.pem",
    "/etc/pki/tls/openssl.cnf",
    "/etc/pki/ca-trust/extracted",
    "/etc/ca-certificates/extracted",
    "/usr/share/ca-certificates",
    "/usr/local/share/ca-certificates",
];

/// Conventional private-key directories. A support path may not be one of
/// these, lie beneath one, or contain one.
#[cfg(target_os = "linux")]
const PRIVATE_KEY_DIRECTORIES: [&str; 2] = ["/etc/ssl/private", "/etc/pki/tls/private"];

/// Whether a resolved support path would expose a private-key directory.
#[cfg(target_os = "linux")]
pub(super) fn exposes_private_keys(path: &Path) -> bool {
    PRIVATE_KEY_DIRECTORIES.iter().any(|private| {
        let private = Path::new(private);
        path.starts_with(private) || private.starts_with(path)
    })
}

/// The fixed resolver and certificate paths a networked Linux run reads, as
/// real paths without symbolic links, which confinement rules require. Paths
/// that do not exist are left out. The caller lists them read-only and should
/// record them, because they expose host configuration such as `/etc/hosts`.
///
/// `/etc/resolv.conf` is often a link into `/run`; the rule then names the
/// file the link pointed to when the run started. A path whose real path is,
/// lies beneath, or contains a private-key directory is left out too.
///
/// # Errors
///
/// Fails when a path exists but cannot be resolved.
#[cfg(target_os = "linux")]
pub fn network_support_paths() -> std::io::Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for listed in NETWORK_SUPPORT_PATHS {
        match std::fs::canonicalize(listed) {
            Ok(path) => {
                if !paths.contains(&path) && !exposes_private_keys(&path) {
                    paths.push(path);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(paths)
}

/// The confinement that was in force for a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppliedConfinement {
    /// No filesystem or network confinement: the run came from [`super::run`].
    Unconfined,
    /// A Landlock ruleset at this ABI version, plus the seccomp socket filter.
    /// ABI 4 and later also deny an explicit TCP bind and, without network
    /// access, TCP connect through Landlock; ABI 6 and later also scope signals and
    /// abstract Unix sockets to the tree.
    Landlock {
        /// The Landlock ABI version the kernel reported and the ruleset used.
        abi: u32,
        /// The Unix-domain socket mode the seccomp filter enforced.
        unix_sockets: UnixSocketAccess,
        /// The IP network access the seccomp filter and ruleset allowed.
        network: NetworkAccess,
    },
    /// An AppContainer derived from [`Confinement::container`], inside the
    /// run's job object. It reaches only granted paths and paths every
    /// application package can reach. Without network access it has no
    /// capabilities and no network, including loopback.
    AppContainer {
        /// The network access its capabilities allowed.
        network: NetworkAccess,
    },
}
