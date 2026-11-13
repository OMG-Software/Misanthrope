//! Windows AppContainer confinement tests. The child fixture runs each probe
//! and prints `PROBE:<name>=<result>`, where the result is `ok`, `denied`
//! (`ERROR_ACCESS_DENIED`), or `os-<code>`. A grandchild repeats two probes to
//! show that descendants stay in the container. The same probes run
//! unconfined as a positive control, so a denial is not a broken probe.
//! Capability probes print `held` or `absent` for the child's own token.

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Security::{
    CheckTokenCapability, WELL_KNOWN_SID_TYPE, WinCapabilityInternetClientSid,
    WinCapabilityPrivateNetworkClientServerSid,
};
use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT,
};

use super::super::{
    AppliedConfinement, CancelSignal, Confinement, NetworkAccess, ProcessError, ProcessOutcome,
    ProcessSpec, ResourceLimit, Termination, UnixSocketAccess, run, run_confined,
};
use super::{FIXTURE_ENV, HARNESS_ARGS, disk_limits, grow_spec, limits, spec, text};
use crate::container::windows::tests::{TestContainer, identity, unique_name};
use crate::container::{ContainerAccess, grant, revoke};
use crate::private_store::windows::Sid;

const READ_ONLY_FILE: &str = "OMGCODE_PROBE_READ_ONLY_FILE";
const SECRET_FILE: &str = "OMGCODE_PROBE_SECRET_FILE";
const WORKSPACE: &str = "OMGCODE_PROBE_WORKSPACE";
const HOST_PIPE: &str = "OMGCODE_PROBE_HOST_PIPE";
const HOST_TCP: &str = "OMGCODE_PROBE_HOST_TCP";
/// `host:port` of an internet service, for the opt-in live probe.
const INTERNET_HOST: &str = "OMGCODE_PROBE_INTERNET_HOST";
const ACCESS_DENIED: i32 = 5;

/// Child side: run every probe and print its result.
pub(super) fn probes() -> i32 {
    let read_only = path_var(READ_ONLY_FILE);
    let secret = path_var(SECRET_FILE);
    let workspace = path_var(WORKSPACE);
    let read_only_directory = read_only.parent().expect("read-only directory");
    let secret_directory = secret.parent().expect("secret directory");

    report("read-allowed", fs::read(&read_only).map(drop));
    report("list-allowed", fs::read_dir(read_only_directory).map(drop));
    report("write-read-only", open_for_write(&read_only));
    report(
        "create-in-read-only",
        fs::write(read_only_directory.join("new.txt"), b"x"),
    );
    report("read-secret", fs::read(&secret).map(drop));
    report("list-secret-dir", fs::read_dir(secret_directory).map(drop));
    report("write-secret", open_for_write(&secret));
    report("workspace-round-trip", workspace_round_trip(&workspace));
    report("temp-round-trip", workspace_round_trip(&env::temp_dir()));
    report("tcp-connect-host", tcp_connect());
    report("pipe-connect-host", pipe_connect());
    capability("internet-client", WinCapabilityInternetClientSid);
    capability(
        "private-network",
        WinCapabilityPrivateNetworkClientServerSid,
    );
    if let Some(host) = env::var_os(INTERNET_HOST) {
        report(
            "internet-connect",
            internet_connect(&host.to_string_lossy()),
        );
    }
    report("grandchild", grandchild());
    0
}

/// Print whether this process's token holds a capability.
#[allow(unsafe_code)]
fn capability(name: &str, kind: WELL_KNOWN_SID_TYPE) {
    let outcome = Sid::well_known(kind).and_then(|sid| {
        let mut held = 0;
        // SAFETY: a null token handle names this thread's or process's token;
        // the SID is valid while `sid` lives and `held` is writable.
        let result =
            unsafe { CheckTokenCapability(std::ptr::null_mut(), sid.as_psid(), &mut held) };
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(held != 0)
    });
    let outcome = match outcome {
        Ok(true) => "held".to_owned(),
        Ok(false) => "absent".to_owned(),
        Err(error) => format!("error-{error}"),
    };
    println!("PROBE:{name}={outcome}");
}

/// Resolve `host` and connect to its first address. The resolver and the
/// connection both need network access.
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
    report("grandchild-tcp-connect-host", tcp_connect());
    0
}

fn path_var(name: &str) -> PathBuf {
    PathBuf::from(env::var_os(name).unwrap_or_else(|| panic!("{name} is not set")))
}

fn report(name: &str, result: io::Result<()>) {
    let outcome = match result {
        Ok(()) => "ok".to_owned(),
        Err(error) => match error.raw_os_error() {
            Some(ACCESS_DENIED) => "denied".to_owned(),
            Some(code) => format!("os-{code}"),
            None => format!("error-{error}"),
        },
    };
    println!("PROBE:{name}={outcome}");
}

fn open_for_write(path: &Path) -> io::Result<()> {
    fs::OpenOptions::new().write(true).open(path).map(drop)
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

/// Windows drops loopback traffic from an AppContainer without a reply, so a
/// plain connect only fails after the system timeout. A short bound keeps the
/// run inside its deadline; any failure counts as denied.
fn tcp_connect() -> io::Result<()> {
    let address: SocketAddr = env::var(HOST_TCP)
        .map_err(io::Error::other)?
        .parse()
        .map_err(io::Error::other)?;
    TcpStream::connect_timeout(&address, Duration::from_secs(2)).map(drop)
}

fn pipe_connect() -> io::Result<()> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path_var(HOST_PIPE))
        .map(drop)
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

/// Create a host named-pipe instance with the default security descriptor,
/// as a service such as a container engine would.
#[allow(unsafe_code)]
fn host_pipe(name: &str) -> OwnedHandle {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is a NUL-terminated pipe name that outlives the call; a
    // null security attribute selects the default descriptor.
    let handle = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            4096,
            4096,
            0,
            std::ptr::null(),
        )
    };
    assert_ne!(
        handle,
        INVALID_HANDLE_VALUE,
        "create pipe: {}",
        io::Error::last_os_error()
    );
    // SAFETY: the handle was just created and nothing else owns it.
    unsafe { OwnedHandle::from_raw_handle(handle) }
}

/// Test side: granted directories, host listeners, and the container.
struct Scene {
    _root: tempfile::TempDir,
    read_only_directory: PathBuf,
    bin: PathBuf,
    program: PathBuf,
    workspace: PathBuf,
    secret: PathBuf,
    pipe_name: String,
    _pipe: OwnedHandle,
    tcp_listener: TcpListener,
    name: TestContainer,
}

impl Scene {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("scene root");
        let read_only_directory = root.path().join("toolchain");
        let bin = root.path().join("bin");
        let workspace = root.path().join("workspace");
        let host = root.path().join("host");
        for directory in [&read_only_directory, &bin, &workspace, &host] {
            fs::create_dir(directory).expect("scene directory");
        }
        fs::write(read_only_directory.join("tool.txt"), b"tool").expect("tool file");
        let secret = host.join("secret.txt");
        fs::write(&secret, b"secret").expect("secret file");
        let program = bin.join("probe.exe");
        fs::copy(env::current_exe().expect("test executable"), &program).expect("copy program");
        let pipe_name = format!(
            r"\\.\pipe\omgcode-probe-{}-{}",
            std::process::id(),
            root.path()
                .file_name()
                .expect("root name")
                .to_string_lossy()
        );
        let pipe = host_pipe(&pipe_name);
        let tcp_listener = TcpListener::bind("127.0.0.1:0").expect("host TCP listener");
        let name = unique_name();
        for (path, access) in [
            (&read_only_directory, ContainerAccess::ReadExecute),
            (&bin, ContainerAccess::ReadExecute),
            (&workspace, ContainerAccess::ReadWrite),
        ] {
            grant(path, identity(path), &name, access).expect("grant");
        }
        Self {
            _root: root,
            read_only_directory,
            bin,
            program,
            workspace,
            secret,
            pipe_name,
            _pipe: pipe,
            tcp_listener,
            name,
        }
    }

    fn confinement(&self) -> Confinement {
        Confinement {
            read_only: vec![self.read_only_directory.clone(), self.bin.clone()],
            read_write: vec![self.workspace.clone()],
            unix_sockets: UnixSocketAccess::PairsOnly,
            network: NetworkAccess::Denied,
            container: Some((*self.name).clone()),
        }
    }

    fn spec(&self) -> ProcessSpec {
        let mut spec = spec("probe");
        spec.program = self.program.clone();
        // Windows replaces this with the container's own folder and sets
        // TEMP and TMP beneath it.
        spec.environment.push((
            OsString::from("LOCALAPPDATA"),
            env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA"),
        ));
        spec.working_directory = self.workspace.clone();
        let tcp = self.tcp_listener.local_addr().expect("TCP address");
        let variables: [(&str, OsString); 5] = [
            (
                READ_ONLY_FILE,
                self.read_only_directory.join("tool.txt").into_os_string(),
            ),
            (SECRET_FILE, self.secret.clone().into_os_string()),
            (WORKSPACE, self.workspace.clone().into_os_string()),
            (HOST_PIPE, OsString::from(&self.pipe_name)),
            (HOST_TCP, OsString::from(tcp.to_string())),
        ];
        spec.environment.extend(
            variables
                .into_iter()
                .map(|(name, value)| (OsString::from(name), value)),
        );
        spec
    }
}

impl Drop for Scene {
    fn drop(&mut self) {
        for path in [&self.read_only_directory, &self.bin, &self.workspace] {
            // The temporary tree is removed next; a failed revoke only leaves
            // an entry on a directory that is about to disappear.
            let _ = revoke(path, identity(path), &self.name);
        }
    }
}

fn results(outcome: &ProcessOutcome) -> BTreeMap<String, String> {
    assert_eq!(
        outcome.termination,
        Termination::Exited { code: 0 },
        "stdout: {}\nstderr: {}",
        text(&outcome.stdout.bytes),
        text(&outcome.stderr.bytes)
    );
    text(&outcome.stdout.bytes)
        .lines()
        .filter_map(|line| line.strip_prefix("PROBE:"))
        .filter_map(|line| line.split_once('='))
        .map(|(name, result)| (name.to_owned(), result.to_owned()))
        .collect()
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

/// Growth in the writable workspace past the disk limit ends the whole
/// contained tree, and the outcome reports the measured peak.
#[test]
fn the_disk_limit_ends_a_confined_run() {
    let scene = Scene::new();
    let growth = scene.workspace.join("growth.bin");
    let spec = grow_spec(scene.spec(), &growth);
    let outcome = run_confined(
        &spec,
        &scene.confinement(),
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
}

#[test]
fn the_container_reaches_only_granted_paths_and_no_host_service() {
    let scene = Scene::new();
    let outcome = run_confined(
        &scene.spec(),
        &scene.confinement(),
        &limits(),
        &CancelSignal::new(),
    )
    .expect("confined run");
    assert_eq!(
        outcome.confinement,
        AppliedConfinement::AppContainer {
            network: NetworkAccess::Denied
        }
    );
    let results = results(&outcome);
    assert_results(
        &results,
        &[
            ("internet-client", "absent"),
            ("private-network", "absent"),
            ("read-allowed", "ok"),
            ("list-allowed", "ok"),
            ("write-read-only", "denied"),
            ("create-in-read-only", "denied"),
            ("read-secret", "denied"),
            ("list-secret-dir", "denied"),
            ("write-secret", "denied"),
            ("workspace-round-trip", "ok"),
            ("temp-round-trip", "ok"),
            ("pipe-connect-host", "denied"),
            ("grandchild", "ok"),
            ("grandchild-read-secret", "denied"),
        ],
    );
    for name in ["tcp-connect-host", "grandchild-tcp-connect-host"] {
        let result = results.get(name).map(String::as_str);
        assert!(
            result.is_some_and(|result| result != "ok"),
            "{name}: {result:?}; all results: {results:?}"
        );
    }
    assert_eq!(fs::read(&scene.secret).expect("secret"), b"secret");
    assert_eq!(
        fs::read(scene.read_only_directory.join("tool.txt")).expect("tool"),
        b"tool",
        "a read-only file changed"
    );
}

#[test]
fn the_same_probes_succeed_unconfined() {
    let scene = Scene::new();
    let outcome = run(&scene.spec(), &limits(), &CancelSignal::new()).expect("unconfined run");
    assert_eq!(outcome.confinement, AppliedConfinement::Unconfined);
    let results = results(&outcome);
    assert_results(
        &results,
        &[
            ("read-allowed", "ok"),
            ("read-secret", "ok"),
            ("list-secret-dir", "ok"),
            ("workspace-round-trip", "ok"),
            ("tcp-connect-host", "ok"),
            ("pipe-connect-host", "ok"),
            ("grandchild", "ok"),
            ("grandchild-read-secret", "ok"),
            ("grandchild-tcp-connect-host", "ok"),
        ],
    );
}

/// With network access the tree holds `internetClient` and nothing more:
/// still no private network, no loopback host service, and no ungranted
/// path.
#[test]
fn a_networked_container_holds_only_internet_client() {
    let scene = Scene::new();
    let mut confinement = scene.confinement();
    confinement.network = NetworkAccess::Internet;
    let outcome = run_confined(&scene.spec(), &confinement, &limits(), &CancelSignal::new())
        .expect("networked run");
    assert_eq!(
        outcome.confinement,
        AppliedConfinement::AppContainer {
            network: NetworkAccess::Internet
        }
    );
    let results = results(&outcome);
    assert_results(
        &results,
        &[
            ("internet-client", "held"),
            ("private-network", "absent"),
            ("read-secret", "denied"),
            ("pipe-connect-host", "denied"),
            ("workspace-round-trip", "ok"),
            ("grandchild", "ok"),
            ("grandchild-read-secret", "denied"),
        ],
    );
    for name in ["tcp-connect-host", "grandchild-tcp-connect-host"] {
        let result = results.get(name).map(String::as_str);
        assert!(
            result.is_some_and(|result| result != "ok"),
            "{name}: {result:?}; all results: {results:?}"
        );
    }
}

/// Live check that a networked container resolves and reaches an internet
/// host. It needs internet access, so it runs only when asked:
/// `OMGCODE_TEST_INTERNET_HOST=example.com:80 cargo test -- --ignored`.
#[test]
#[ignore = "needs internet access and OMGCODE_TEST_INTERNET_HOST"]
fn a_networked_container_reaches_the_internet() {
    let host = env::var_os("OMGCODE_TEST_INTERNET_HOST").expect("OMGCODE_TEST_INTERNET_HOST");
    let scene = Scene::new();
    let mut spec = scene.spec();
    spec.environment
        .push((OsString::from(INTERNET_HOST), host.clone()));
    let mut confinement = scene.confinement();
    let denied = run_confined(&spec, &confinement, &limits(), &CancelSignal::new())
        .expect("run without network");
    let denied = results(&denied);
    assert_ne!(
        denied.get("internet-connect").map(String::as_str),
        Some("ok"),
        "reached {host:?} without network access"
    );
    confinement.network = NetworkAccess::Internet;
    let allowed =
        run_confined(&spec, &confinement, &limits(), &CancelSignal::new()).expect("networked run");
    assert_results(&results(&allowed), &[("internet-connect", "ok")]);
}

#[test]
fn missing_or_weaker_grants_start_nothing() {
    let scene = Scene::new();
    let record = scene.workspace.join("started.txt");
    let mut cases = Vec::new();
    let mut ungranted = scene.confinement();
    ungranted.read_only.push(scene.secret.clone());
    cases.push(("ungranted path", ungranted));
    let mut weaker = scene.confinement();
    weaker.read_write.push(scene.read_only_directory.clone());
    cases.push(("read-only grant listed as read-write", weaker));
    let mut other_container = scene.confinement();
    other_container.container = Some((*unique_name()).clone());
    cases.push(("another container", other_container));
    for (name, confinement) in cases {
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

#[test]
fn windows_refuses_host_services_and_incomplete_confinement() {
    let scene = Scene::new();
    let mut host_services = scene.confinement();
    host_services.unix_sockets = UnixSocketAccess::HostServices;
    let error = run_confined(
        &scene.spec(),
        &host_services,
        &limits(),
        &CancelSignal::new(),
    )
    .expect_err("host services");
    assert!(matches!(error, ProcessError::Unsupported(_)), "{error}");
    let mut unnamed = scene.confinement();
    unnamed.container = None;
    let error = run_confined(&scene.spec(), &unnamed, &limits(), &CancelSignal::new())
        .expect_err("no container");
    assert!(matches!(error, ProcessError::InvalidSpec(_)), "{error}");
    let mut spec = scene.spec();
    spec.environment
        .retain(|(name, _)| !name.eq_ignore_ascii_case("LOCALAPPDATA"));
    let error = run_confined(&spec, &scene.confinement(), &limits(), &CancelSignal::new())
        .expect_err("no LOCALAPPDATA");
    assert!(matches!(error, ProcessError::InvalidSpec(_)), "{error}");
    let missing_profile = Confinement {
        container: Some((*unique_name()).clone()),
        read_only: Vec::new(),
        read_write: Vec::new(),
        ..scene.confinement()
    };
    let error = run_confined(
        &scene.spec(),
        &missing_profile,
        &limits(),
        &CancelSignal::new(),
    )
    .expect_err("no profile");
    assert!(
        matches!(&error, ProcessError::Confinement(cause) if cause.kind() == io::ErrorKind::NotFound),
        "{error}"
    );
}

#[test]
fn a_program_the_container_cannot_read_does_not_start() {
    let scene = Scene::new();
    let mut spec = scene.spec();
    spec.program = env::current_exe().expect("test executable");
    let error = run_confined(&spec, &scene.confinement(), &limits(), &CancelSignal::new())
        .expect_err("the program is not granted to the container");
    assert!(
        matches!(&error, ProcessError::Confinement(cause) if cause.kind() == io::ErrorKind::PermissionDenied),
        "{error}"
    );
}
