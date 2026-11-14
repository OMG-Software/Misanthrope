//! Linux confinement tests. The child fixture runs each probe under
//! `run_confined` and prints `PROBE:<name>=<result>`, where the result is
//! `ok`, `EACCES` (Landlock), `EPERM` (seccomp), or another errno. A grandchild
//! repeats two probes to show that descendants inherit the restrictions.

use std::collections::BTreeMap;
use std::env;
use std::ffi::{CString, OsString};
use std::fs;
use std::io;
use std::net::{TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::super::{
    AppliedConfinement, CancelSignal, Confinement, NetworkAccess, ProcessError, ProcessSpec,
    ResourceLimit, Termination, UnixSocketAccess, network_support_paths, run_confined,
};
use super::{
    FIXTURE_ENV, HARNESS_ARGS, disk_limits, grow_spec, in_limit_scope, limits, spec, text,
};

const READ_ONLY_FILE: &str = "OMGCODE_PROBE_READ_ONLY_FILE";
const SECRET_FILE: &str = "OMGCODE_PROBE_SECRET_FILE";
const WORKSPACE: &str = "OMGCODE_PROBE_WORKSPACE";
const HOST_SOCKET: &str = "OMGCODE_PROBE_HOST_SOCKET";
const HOST_TCP: &str = "OMGCODE_PROBE_HOST_TCP";
/// `host:port` of an internet service, for the opt-in live probe.
const INTERNET_HOST: &str = "OMGCODE_PROBE_INTERNET_HOST";
/// Files the child reads, as a path list: public certificates and private
/// keys in a host tree the test builds.
const PUBLIC_FILES: &str = "OMGCODE_PROBE_PUBLIC_FILES";
const PRIVATE_FILES: &str = "OMGCODE_PROBE_PRIVATE_FILES";

/// Host certificate directories that the probes list, by probe name: the
/// public stores that network access grants, the private-key directories it
/// must not, and their parents. A directory that does not exist is skipped.
const CERTIFICATE_PROBES: [(&str, &str); 6] = [
    ("list-ssl-certs", "/etc/ssl/certs"),
    ("list-ssl", "/etc/ssl"),
    ("list-ssl-private", "/etc/ssl/private"),
    ("list-pki-certs", "/etc/pki/tls/certs"),
    ("list-pki", "/etc/pki"),
    ("list-pki-private", "/etc/pki/tls/private"),
];

/// Child side: run every probe and print its result.
pub(super) fn probes() -> i32 {
    let read_only = path_var(READ_ONLY_FILE);
    let secret = path_var(SECRET_FILE);
    let workspace = path_var(WORKSPACE);
    let host_socket = path_var(HOST_SOCKET);
    let host_tcp = env::var(HOST_TCP).expect("host TCP address");
    let read_only_directory = read_only.parent().expect("read-only directory");
    let secret_directory = secret.parent().expect("secret directory");

    report("read-allowed", fs::read(&read_only).map(drop));
    report("list-allowed", fs::read_dir(read_only_directory).map(drop));
    report("write-read-only", open_for_write(&read_only));
    report("truncate-read-only", truncate_path(&read_only));
    report(
        "create-in-read-only",
        fs::write(read_only_directory.join("new.txt"), b"x"),
    );
    report("read-secret", fs::read(&secret).map(drop));
    report("list-secret-dir", fs::read_dir(secret_directory).map(drop));
    report("write-secret", open_for_write(&secret));
    report("workspace-round-trip", workspace_round_trip(&workspace));
    report("tcp-connect-host", TcpStream::connect(&host_tcp).map(drop));
    report("udp-bind", UdpSocket::bind("127.0.0.1:0").map(drop));
    report("tcp6-bind", TcpListener::bind("[::1]:0").map(drop));
    report("unix-connect-host", unix_connect(&host_socket));
    report(
        "unix-bind-workspace",
        UnixListener::bind(workspace.join("own.sock")).map(drop),
    );
    report("unix-stream-pair", UnixStream::pair().map(drop));
    report("unix-datagram-pair", UnixDatagram::pair().map(drop));
    report("io-uring-setup", io_uring_setup());
    for (name, directory) in CERTIFICATE_PROBES {
        if Path::new(directory).is_dir() {
            report(name, fs::read_dir(directory).map(drop));
        }
    }
    for (variable, prefix) in [
        (PUBLIC_FILES, "read-public"),
        (PRIVATE_FILES, "read-private"),
    ] {
        if let Some(files) = env::var_os(variable) {
            for (index, file) in env::split_paths(&files).enumerate() {
                report(&format!("{prefix}-{index}"), fs::read(file).map(drop));
            }
        }
    }
    if let Some(host) = env::var_os(INTERNET_HOST) {
        report(
            "internet-connect",
            internet_connect(&host.to_string_lossy()),
        );
    }
    report("grandchild", grandchild());
    0
}

/// Resolve `host` and connect to its first address. The resolver and the
/// connection both need network access and the resolver files.
fn internet_connect(host: &str) -> io::Result<()> {
    let address = host
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::other("the host has no address"))?;
    TcpStream::connect_timeout(&address, Duration::from_secs(10)).map(drop)
}

/// Grandchild side: two probes, then exit.
pub(super) fn grandchild_probes() -> i32 {
    report(
        "grandchild-read-secret",
        fs::read(path_var(SECRET_FILE)).map(drop),
    );
    let host_tcp = env::var(HOST_TCP).expect("host TCP address");
    report(
        "grandchild-tcp-connect-host",
        TcpStream::connect(host_tcp).map(drop),
    );
    0
}

fn path_var(name: &str) -> PathBuf {
    PathBuf::from(env::var_os(name).unwrap_or_else(|| panic!("{name} is not set")))
}

fn report(name: &str, result: io::Result<()>) {
    let outcome = match result {
        Ok(()) => "ok".to_owned(),
        Err(error) => match error.raw_os_error() {
            Some(libc::EACCES) => "EACCES".to_owned(),
            Some(libc::EPERM) => "EPERM".to_owned(),
            Some(code) => format!("errno-{code}"),
            None => format!("error-{error}"),
        },
    };
    println!("PROBE:{name}={outcome}");
}

fn open_for_write(path: &Path) -> io::Result<()> {
    fs::OpenOptions::new().write(true).open(path).map(drop)
}

#[allow(unsafe_code)]
fn truncate_path(path: &Path) -> io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
    // SAFETY: `path` is a valid NUL-terminated string for the call; truncate
    // reads it and writes no memory.
    if unsafe { libc::truncate(path.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn workspace_round_trip(workspace: &Path) -> io::Result<()> {
    let file = workspace.join("written.txt");
    fs::write(&file, b"workspace")?;
    if fs::read(&file)? != b"workspace" {
        return Err(io::Error::other("read back different bytes"));
    }
    let directory = workspace.join("made");
    fs::create_dir(&directory)?;
    let moved = directory.join("moved.txt");
    fs::rename(&file, &moved)?;
    fs::remove_file(&moved)?;
    fs::remove_dir(&directory)
}

/// Connect to the host listener. The listener's backlog completes the
/// connection, so `ok` means a socket outside the tree was reached.
fn unix_connect(path: &Path) -> io::Result<()> {
    UnixStream::connect(path).map(drop)
}

#[allow(unsafe_code)]
fn io_uring_setup() -> io::Result<()> {
    // `struct io_uring_params` is 120 bytes; zero requests no features.
    let mut params = [0u8; 120];
    // SAFETY: `params` is a writable buffer of the size the kernel expects
    // and outlives the call.
    let result = unsafe {
        libc::syscall(
            libc::SYS_io_uring_setup,
            1 as libc::c_uint,
            params.as_mut_ptr(),
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = libc::c_int::try_from(result).map_err(io::Error::other)?;
    // SAFETY: the kernel returned a new descriptor that nothing else owns.
    unsafe { libc::close(descriptor) };
    Ok(())
}

fn grandchild() -> io::Result<()> {
    let status = Command::new(env::current_exe()?)
        .args(HARNESS_ARGS)
        .env(FIXTURE_ENV, "probe-grandchild")
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("grandchild failed: {status}")))
    }
}

/// Test side: private directories, host listeners, and the confinement.
struct Scene {
    root: tempfile::TempDir,
    read_only_directory: PathBuf,
    workspace: PathBuf,
    secret: PathBuf,
    host_socket: PathBuf,
    _unix_listener: UnixListener,
    tcp_listener: TcpListener,
}

impl Scene {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("scene root");
        let read_only_directory = root.path().join("toolchain");
        let workspace = root.path().join("workspace");
        let host = root.path().join("host");
        for directory in [&read_only_directory, &workspace, &host] {
            fs::create_dir(directory).expect("scene directory");
        }
        fs::write(read_only_directory.join("tool.txt"), b"tool").expect("tool file");
        let secret = host.join("secret.txt");
        fs::write(&secret, b"secret").expect("secret file");
        let host_socket = host.join("service.sock");
        let unix_listener = UnixListener::bind(&host_socket).expect("host Unix listener");
        let tcp_listener = TcpListener::bind("127.0.0.1:0").expect("host TCP listener");
        Self {
            root,
            read_only_directory,
            workspace,
            secret,
            host_socket,
            _unix_listener: unix_listener,
            tcp_listener,
        }
    }

    fn confinement(&self, unix_sockets: UnixSocketAccess, network: NetworkAccess) -> Confinement {
        let mut read_only = vec![
            PathBuf::from("/usr"),
            self.read_only_directory.clone(),
            executable_directory(),
        ];
        let cache = PathBuf::from("/etc/ld.so.cache");
        if cache.exists() {
            read_only.push(cache);
        }
        if network == NetworkAccess::Internet {
            read_only.extend(network_support_paths().expect("network support paths"));
        }
        Confinement {
            read_only,
            read_write: vec![self.workspace.clone()],
            unix_sockets,
            network,
            container: None,
        }
    }

    fn spec(&self) -> ProcessSpec {
        let mut spec = spec("probe");
        spec.working_directory = self.workspace.clone();
        let tcp = self.tcp_listener.local_addr().expect("TCP address");
        let variables: [(&str, OsString); 5] = [
            (
                READ_ONLY_FILE,
                self.read_only_directory.join("tool.txt").into_os_string(),
            ),
            (SECRET_FILE, self.secret.clone().into_os_string()),
            (WORKSPACE, self.workspace.clone().into_os_string()),
            (HOST_SOCKET, self.host_socket.clone().into_os_string()),
            (HOST_TCP, OsString::from(tcp.to_string())),
        ];
        spec.environment.extend(
            variables
                .into_iter()
                .map(|(name, value)| (OsString::from(name), value)),
        );
        spec
    }

    fn run(
        &self,
        unix_sockets: UnixSocketAccess,
        network: NetworkAccess,
    ) -> (AppliedConfinement, BTreeMap<String, String>) {
        self.run_spec(&self.spec(), unix_sockets, network)
    }

    fn run_spec(
        &self,
        spec: &ProcessSpec,
        unix_sockets: UnixSocketAccess,
        network: NetworkAccess,
    ) -> (AppliedConfinement, BTreeMap<String, String>) {
        Self::run_with(spec, &self.confinement(unix_sockets, network))
    }

    fn run_with(
        spec: &ProcessSpec,
        confinement: &Confinement,
    ) -> (AppliedConfinement, BTreeMap<String, String>) {
        let outcome =
            run_confined(spec, confinement, &limits(), &CancelSignal::new()).expect("confined run");
        assert_eq!(
            outcome.termination,
            Termination::Exited { code: 0 },
            "stdout: {}\nstderr: {}",
            text(&outcome.stdout.bytes),
            text(&outcome.stderr.bytes)
        );
        let results = text(&outcome.stdout.bytes)
            .lines()
            .filter_map(|line| line.strip_prefix("PROBE:"))
            .filter_map(|line| line.split_once('='))
            .map(|(name, result)| (name.to_owned(), result.to_owned()))
            .collect();
        (outcome.confinement, results)
    }
}

fn executable_directory() -> PathBuf {
    env::current_exe()
        .expect("test executable")
        .parent()
        .expect("executable directory")
        .to_path_buf()
}

fn assert_results(results: &BTreeMap<String, String>, expected: &[(&str, &str)]) {
    for (name, result) in expected {
        assert_eq!(
            results.get(*name).map(String::as_str),
            Some(*result),
            "probe {name}; all results: {results:?}"
        );
    }
}

/// Results that must hold in every Unix-socket and network mode.
const COMMON: [(&str, &str); 11] = [
    ("read-allowed", "ok"),
    ("list-allowed", "ok"),
    ("write-read-only", "EACCES"),
    ("truncate-read-only", "EACCES"),
    ("create-in-read-only", "EACCES"),
    ("read-secret", "EACCES"),
    ("list-secret-dir", "EACCES"),
    ("write-secret", "EACCES"),
    ("workspace-round-trip", "ok"),
    ("unix-stream-pair", "ok"),
    ("io-uring-setup", "EPERM"),
];

/// Results that must hold without network access.
const NO_NETWORK: [(&str, &str); 4] = [
    ("tcp-connect-host", "EPERM"),
    ("udp-bind", "EPERM"),
    ("tcp6-bind", "EPERM"),
    ("grandchild-tcp-connect-host", "EPERM"),
];

const GRANDCHILD: [(&str, &str); 2] = [("grandchild", "ok"), ("grandchild-read-secret", "EACCES")];

#[test]
fn the_default_mode_confines_files_and_refuses_every_host_socket() {
    let scene = Scene::new();
    let (applied, results) = scene.run(UnixSocketAccess::PairsOnly, NetworkAccess::Denied);
    assert!(
        matches!(
            applied,
            AppliedConfinement::Landlock {
                abi: 3..,
                unix_sockets: UnixSocketAccess::PairsOnly,
                network: NetworkAccess::Denied,
            }
        ),
        "{applied:?}"
    );
    assert_results(&results, &COMMON);
    assert_results(&results, &NO_NETWORK);
    assert_results(&results, &GRANDCHILD);
    assert_results(
        &results,
        &[
            ("unix-connect-host", "EPERM"),
            ("unix-bind-workspace", "EPERM"),
            ("unix-datagram-pair", "EPERM"),
        ],
    );
    assert_eq!(fs::read(&scene.secret).expect("secret"), b"secret");
    assert_eq!(
        fs::read(scene.read_only_directory.join("tool.txt")).expect("tool"),
        b"tool",
        "a read-only file changed"
    );
}

#[test]
fn host_services_mode_reaches_unix_sockets_but_not_ip_or_files() {
    let scene = Scene::new();
    let (applied, results) = scene.run(UnixSocketAccess::HostServices, NetworkAccess::Denied);
    assert!(
        matches!(
            applied,
            AppliedConfinement::Landlock {
                abi: 3..,
                unix_sockets: UnixSocketAccess::HostServices,
                network: NetworkAccess::Denied,
            }
        ),
        "{applied:?}"
    );
    assert_results(&results, &COMMON);
    assert_results(&results, &NO_NETWORK);
    assert_results(&results, &GRANDCHILD);
    assert_results(
        &results,
        &[
            ("unix-connect-host", "ok"),
            ("unix-bind-workspace", "ok"),
            ("unix-datagram-pair", "ok"),
        ],
    );
}

/// Network access opens IP sockets to every destination, loopback included,
/// for the whole tree. Files and Unix sockets keep their rules. On Landlock
/// ABI 4 and later, an explicit TCP bind stays denied.
#[test]
fn network_access_reaches_ip_but_keeps_files_and_unix_rules() {
    let scene = Scene::new();
    let (applied, results) = scene.run(UnixSocketAccess::PairsOnly, NetworkAccess::Internet);
    let AppliedConfinement::Landlock {
        abi,
        unix_sockets: UnixSocketAccess::PairsOnly,
        network: NetworkAccess::Internet,
    } = applied
    else {
        panic!("{applied:?}");
    };
    assert_results(&results, &COMMON);
    assert_results(&results, &GRANDCHILD);
    let tcp_bind = if abi >= 4 { "EACCES" } else { "ok" };
    assert_results(
        &results,
        &[
            ("tcp-connect-host", "ok"),
            ("grandchild-tcp-connect-host", "ok"),
            ("udp-bind", "ok"),
            ("tcp6-bind", tcp_bind),
            ("unix-connect-host", "EPERM"),
            ("unix-bind-workspace", "EPERM"),
            ("unix-datagram-pair", "EPERM"),
        ],
    );
}

/// A networked run can list the public certificate stores but not a
/// private-key directory or the parent that holds one. Where this test
/// process, unconfined, can list a denied directory, the denial comes from
/// Landlock rather than the directory's own permissions.
#[test]
fn a_networked_run_cannot_reach_private_key_directories() {
    let scene = Scene::new();
    let (_, results) = scene.run(UnixSocketAccess::PairsOnly, NetworkAccess::Internet);
    let mut attributed = 0;
    for (name, directory) in CERTIFICATE_PROBES {
        if !Path::new(directory).is_dir() {
            assert!(!results.contains_key(name), "{name}: {results:?}");
            continue;
        }
        if name.ends_with("-certs") {
            assert_results(&results, &[(name, "ok")]);
            continue;
        }
        assert_results(&results, &[(name, "EACCES")]);
        if fs::read_dir(directory).is_ok() {
            attributed += 1;
        }
    }
    if Path::new("/etc/ssl").is_dir() || Path::new("/etc/pki").is_dir() {
        assert!(attributed > 0, "no denial could be attributed to Landlock");
    }
}

/// The support-path rules applied to a host tree the test builds, with a
/// private key the user can read beside each public certificate store. A
/// networked child reads the certificates but not the keys, which this test
/// process reads unconfined.
#[test]
fn a_networked_run_reads_certificates_but_not_private_keys() {
    use crate::process::confinement::support_paths_under;

    let host = tempfile::tempdir().expect("host tree");
    let file = |relative: &str, contents: &[u8]| {
        let path = host.path().join(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("directories");
        fs::write(&path, contents).expect("host file");
        fs::canonicalize(path).expect("real path")
    };
    let public = [
        file("etc/ssl/certs/ca-certificates.crt", b"certificate"),
        file("etc/ssl/openssl.cnf", b"configuration"),
        file("etc/pki/tls/certs/ca-bundle.crt", b"certificate"),
    ];
    let private = [
        file("etc/ssl/private/host.key", b"private key"),
        file("etc/pki/tls/private/host.key", b"private key"),
    ];
    for key in &private {
        fs::read(key).expect("the key is readable unconfined");
    }
    let scene = Scene::new();
    let mut confinement = scene.confinement(UnixSocketAccess::PairsOnly, NetworkAccess::Denied);
    confinement.network = NetworkAccess::Internet;
    confinement
        .read_only
        .extend(support_paths_under(host.path()).expect("support paths"));
    let mut spec = scene.spec();
    for (variable, files) in [(PUBLIC_FILES, &public[..]), (PRIVATE_FILES, &private[..])] {
        spec.environment.push((
            OsString::from(variable),
            env::join_paths(files).expect("path list"),
        ));
    }
    let (_, results) = Scene::run_with(&spec, &confinement);
    for index in 0..public.len() {
        assert_results(&results, &[(&format!("read-public-{index}"), "ok")]);
    }
    for index in 0..private.len() {
        assert_results(&results, &[(&format!("read-private-{index}"), "EACCES")]);
    }
}

/// Certificate access never includes a private-key directory: not the
/// directory, anything beneath it, or a parent that contains it.
#[test]
fn support_paths_never_expose_private_keys() {
    use crate::process::confinement::exposes_private_keys;

    for exposed in [
        "/etc",
        "/etc/ssl",
        "/etc/pki",
        "/etc/pki/tls",
        "/etc/ssl/private",
        "/etc/ssl/private/host.key",
        "/etc/pki/tls/private",
    ] {
        assert!(exposes_private_keys(Path::new(exposed)), "{exposed}");
    }
    for public in [
        "/etc/ssl/certs",
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/ssl/openssl.cnf",
        "/etc/pki/tls/certs",
        "/etc/pki/ca-trust/extracted",
        "/usr/share/ca-certificates",
        "/etc/hosts",
    ] {
        assert!(!exposes_private_keys(Path::new(public)), "{public}");
    }
    let paths = network_support_paths().expect("network support paths");
    assert!(
        !paths.iter().any(|path| exposes_private_keys(path)),
        "{paths:?}"
    );
}

#[test]
fn network_support_paths_are_real_and_exist() {
    let paths = network_support_paths().expect("network support paths");
    for path in &paths {
        assert!(path.is_absolute(), "{path:?}");
        assert_eq!(
            &fs::canonicalize(path).expect("resolve"),
            path,
            "{path:?} is not a real path"
        );
    }
    if Path::new("/etc/hosts").exists() {
        assert!(!paths.is_empty());
    }
}

/// Live check that a networked run resolves and reaches an internet host
/// through the support paths. It needs internet access, so it runs only when
/// asked: `OMGCODE_TEST_INTERNET_HOST=example.com:80 cargo test -- --ignored`.
#[test]
#[ignore = "needs internet access and OMGCODE_TEST_INTERNET_HOST"]
fn a_networked_run_reaches_the_internet() {
    let host = env::var_os("OMGCODE_TEST_INTERNET_HOST").expect("OMGCODE_TEST_INTERNET_HOST");
    let scene = Scene::new();
    let mut spec = scene.spec();
    spec.environment
        .push((OsString::from(INTERNET_HOST), host.clone()));
    let (_, denied) = scene.run_spec(&spec, UnixSocketAccess::PairsOnly, NetworkAccess::Denied);
    assert_ne!(
        denied.get("internet-connect").map(String::as_str),
        Some("ok"),
        "reached {host:?} without network access"
    );
    let (_, allowed) = scene.run_spec(&spec, UnixSocketAccess::PairsOnly, NetworkAccess::Internet);
    assert_results(&allowed, &[("internet-connect", "ok")]);
}

/// Growth in the writable workspace past the disk limit ends the whole
/// confined tree, and the outcome reports the measured peak.
#[test]
fn the_disk_limit_ends_a_confined_run() {
    in_limit_scope(
        "process::tests::confinement::the_disk_limit_ends_a_confined_run",
        || {
            let scene = Scene::new();
            let growth = scene.workspace.join("growth.bin");
            let spec = grow_spec(scene.spec(), &growth);
            let confinement = scene.confinement(UnixSocketAccess::PairsOnly, NetworkAccess::Denied);
            let outcome = run_confined(
                &spec,
                &confinement,
                &disk_limits(16 * 1024 * 1024),
                &CancelSignal::new(),
            )
            .expect("confined run");
            assert_eq!(
                outcome.termination,
                Termination::LimitExceeded {
                    limit: ResourceLimit::Disk
                }
            );
            let usage = outcome.usage.expect("a limited run reports usage");
            assert!(
                usage.peak_disk_bytes.expect("disk usage") >= 16 * 1024 * 1024,
                "{usage:?}"
            );
        },
    );
}

#[test]
fn a_program_outside_the_rules_cannot_start() {
    let scene = Scene::new();
    let mut confinement = scene.confinement(UnixSocketAccess::PairsOnly, NetworkAccess::Denied);
    let executable = executable_directory();
    confinement
        .read_only
        .retain(|path| path.as_path() != executable);
    let error = run_confined(&scene.spec(), &confinement, &limits(), &CancelSignal::new())
        .expect_err("the program is not executable under the rules");
    assert!(
        matches!(&error, ProcessError::Spawn(cause) if cause.raw_os_error() == Some(libc::EACCES)),
        "{error}"
    );
}

#[test]
fn unusable_confinement_paths_start_nothing() {
    let scene = Scene::new();
    let base = scene.root.path();
    let link = base.join("link");
    std::os::unix::fs::symlink(&scene.workspace, &link).expect("symlink");
    let fifo = base.join("fifo");
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .expect("fifo");
    let cases = [
        ("missing", base.join("missing")),
        ("final symlink", link.clone()),
        ("symlink component", link.join("nested")),
        ("fifo", fifo),
    ];
    fs::create_dir(scene.workspace.join("nested")).expect("nested directory");
    let record = scene.workspace.join("started.txt");
    for (name, path) in cases {
        let mut confinement = scene.confinement(UnixSocketAccess::PairsOnly, NetworkAccess::Denied);
        confinement.read_write.push(path);
        let mut spec = scene.spec();
        spec.environment
            .retain(|(variable, _)| variable != FIXTURE_ENV);
        spec.environment
            .push((OsString::from(FIXTURE_ENV), OsString::from("ready-sleep")));
        spec.environment.push((
            OsString::from(super::RECORD_ENV),
            record.clone().into_os_string(),
        ));
        let error =
            run_confined(&spec, &confinement, &limits(), &CancelSignal::new()).expect_err(name);
        assert!(
            matches!(error, ProcessError::Confinement(_)),
            "{name}: {error}"
        );
        assert!(!record.exists(), "{name}: the program started");
    }
}
