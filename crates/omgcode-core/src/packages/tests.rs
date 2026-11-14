use super::*;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::control::{ControlStore, PackageProfileDeclaration};
use crate::policy::{PermissionSet, WorkspaceId};

const PROFILE: &str = "registries";
const MAX_REQUEST_HEADER_BYTES: usize = 16 * 1024;

/// One bounded local HTTP response.
struct Fixture {
    address: SocketAddr,
    task: JoinHandle<(String, usize)>,
}

impl Fixture {
    fn start(status: u16, body: Vec<u8>, headers: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        listener
            .set_nonblocking(true)
            .expect("make fixture accept bounded");
        let address = listener.local_addr().expect("fixture address");
        let headers = headers.to_owned();
        let task = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "fixture accept timed out");
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                }
            };
            stream
                .set_nonblocking(false)
                .expect("make accepted stream blocking");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .expect("write timeout");
            let head = read_head(&mut stream);
            let response = format!(
                "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response headers");
            stream.write_all(&body).expect("write response body");
            (head, body.len())
        });
        Self { address, task }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    fn finish(self) -> String {
        join_bounded(self.task)
    }
}

fn read_head(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 1024];
    while bytes.len() < MAX_REQUEST_HEADER_BYTES {
        let read = stream.read(&mut buffer).expect("read request");
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(bytes).expect("request head is UTF-8")
}

fn join_bounded(task: JoinHandle<(String, usize)>) -> String {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !task.is_finished() {
        assert!(Instant::now() < deadline, "fixture finished");
        thread::sleep(Duration::from_millis(2));
    }
    let (head, _) = task.join().expect("fixture joined");
    head
}

/// A workspace with `task_network`, a package profile, and the control data.
struct Scene {
    _directory: tempfile::TempDir,
    data: PathBuf,
    workspace_id: WorkspaceId,
}

impl Scene {
    fn new(origins: &[String]) -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let data = directory.path().join("data");
        let root = directory.path().join("dev");
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        let mut store = ControlStore::open(&data).expect("store");
        store.set_development_root(&root, 0).expect("root");
        let workspace_id = store
            .register_workspace(&workspace, store.revision())
            .expect("register")
            .id();
        let permissions = PermissionSet {
            task_network: true,
            ..PermissionSet::default()
        };
        store
            .set_permissions(workspace_id, permissions, store.revision())
            .expect("permissions");
        store
            .add_package_profile(
                &PackageProfileDeclaration {
                    name: PROFILE.into(),
                    origins: origins.to_vec(),
                },
                store.revision(),
            )
            .expect("profile");
        Self {
            _directory: directory,
            data,
            workspace_id,
        }
    }

    fn request(&self, url: String, name: &str, max_bytes: u64) -> FetchRequest {
        FetchRequest {
            workspace_id: self.workspace_id,
            profile: PROFILE.into(),
            url,
            name: name.into(),
            max_bytes,
            timeout: Duration::from_secs(10),
        }
    }

    fn cache_file(&self, name: &str) -> PathBuf {
        self.data
            .join("package-cache")
            .join(self.workspace_id.to_string())
            .join(name)
    }

    fn audit_text(&self) -> String {
        let store = ControlStore::open_existing(&self.data)
            .expect("open")
            .expect("store");
        serde_json::to_string(&store.audit(100).expect("audit")).expect("audit JSON")
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

#[test]
fn a_declared_origin_is_fetched_and_audited() {
    let body = b"package-bytes".to_vec();
    let fixture = Fixture::start(200, body.clone(), "");
    let scene = Scene::new(&[format!("http://{}/fixtures/", fixture.address)]);
    let url = fixture.url("/fixtures/pkg.bin");
    let report = fetch(&scene.data, &scene.request(url.clone(), "pkg.bin", 1024))
        .expect("fetch allowed origin");
    assert_eq!(report.bytes, body.len() as u64);
    assert_eq!(report.sha256, hex_digest(&body));
    assert_eq!(report.status, 200);
    assert_eq!(report.profile_name, PROFILE);
    assert_eq!(report.url_sha256, hex_digest(url.as_bytes()));
    assert_eq!(
        fs::read(scene.cache_file("pkg.bin")).expect("cache file"),
        body
    );
    let head = fixture.finish();
    assert!(head.starts_with("GET /fixtures/pkg.bin "), "{head}");

    let audit = scene.audit_text();
    assert!(audit.contains("package_fetch_started"), "{audit}");
    assert!(audit.contains("package_fetch_finished"), "{audit}");
    assert!(
        !audit.contains(&url),
        "the request URL was recorded: {audit}"
    );
    assert!(audit.contains(&report.url_sha256), "{audit}");
    assert!(audit.contains("\"kind\":\"fetched\""), "{audit}");
}

#[test]
fn a_configured_cache_root_receives_the_fetch() {
    let body = b"external-cache".to_vec();
    let fixture = Fixture::start(200, body.clone(), "");
    let scene = Scene::new(&[format!("http://{}/fixtures/", fixture.address)]);
    let cache_root = scene._directory.path().join("external-cache");
    fs::create_dir(&cache_root).expect("cache root");
    let mut store = ControlStore::open(&scene.data).expect("store");
    store
        .set_package_cache_root(scene.workspace_id, &cache_root, store.revision())
        .expect("cache root");
    drop(store);
    let url = fixture.url("/fixtures/pkg.bin");
    let report = fetch(&scene.data, &scene.request(url, "pkg.bin", 1024)).expect("fetch");
    assert_eq!(report.bytes, body.len() as u64);
    let cached = cache_root.join("pkg.bin");
    assert_eq!(fs::read(cached).expect("external cache file"), body);
    assert!(!scene.cache_file("pkg.bin").exists());
    let head = fixture.finish();
    assert!(head.starts_with("GET /fixtures/pkg.bin "), "{head}");
}

#[test]
fn an_undeclared_origin_or_path_is_refused_before_a_request() {
    // A closed loopback port stands in for an origin that was not declared.
    let scene = Scene::new(&["http://127.0.0.1:9/fixtures/".to_owned()]);
    let before = scene.audit_text();
    let error = fetch(
        &scene.data,
        &scene.request(
            "http://127.0.0.1:9/other/pkg.bin".to_owned(),
            "pkg.bin",
            1024,
        ),
    )
    .expect_err("undeclared path");
    assert!(matches!(error, FetchError::Refused(_)), "{error}");
    assert_eq!(scene.audit_text(), before);
    assert!(!scene.cache_file("pkg.bin").exists());

    let error = fetch(
        &scene.data,
        &scene.request(
            "http://127.0.0.1:10/fixtures/pkg.bin".to_owned(),
            "pkg.bin",
            1024,
        ),
    )
    .expect_err("undeclared origin");
    assert!(matches!(error, FetchError::Refused(_)), "{error}");
    assert_eq!(scene.audit_text(), before);
}

#[test]
fn redirects_are_not_followed() {
    let fixture = Fixture::start(302, Vec::new(), "Location: /elsewhere\r\n");
    let scene = Scene::new(&[format!("http://{}/fixtures/", fixture.address)]);
    let url = fixture.url("/fixtures/pkg.bin");
    let error = fetch(&scene.data, &scene.request(url, "pkg.bin", 1024)).expect_err("redirect");
    assert!(matches!(error, FetchError::Status(302)), "{error}");
    assert!(!scene.cache_file("pkg.bin").exists());
    let audit = scene.audit_text();
    assert!(audit.contains("\"kind\":\"failed\""), "{audit}");
    assert!(audit.contains("status"), "{audit}");
}

#[test]
fn the_size_limit_is_enforced_and_leaves_no_cache_file() {
    let fixture = Fixture::start(200, b"too-large".to_vec(), "");
    let scene = Scene::new(&[format!("http://{}/fixtures/", fixture.address)]);
    let url = fixture.url("/fixtures/pkg.bin");
    let error = fetch(&scene.data, &scene.request(url, "pkg.bin", 4)).expect_err("oversize");
    assert!(matches!(error, FetchError::TooLarge(4)), "{error}");
    assert!(!scene.cache_file("pkg.bin").exists());
    let listing: Vec<_> = fs::read_dir(scene.cache_file("pkg.bin").parent().expect("cache"))
        .expect("cache listing")
        .collect();
    assert!(listing.is_empty(), "a partial cache file remained");
}

#[test]
fn an_existing_cache_file_is_not_overwritten() {
    let body = b"first".to_vec();
    let fixture = Fixture::start(200, body.clone(), "");
    let scene = Scene::new(&[format!("http://{}/fixtures/", fixture.address)]);
    let url = fixture.url("/fixtures/pkg.bin");
    fetch(&scene.data, &scene.request(url.clone(), "pkg.bin", 1024)).expect("first fetch");
    let before = scene.audit_text();
    let error =
        fetch(&scene.data, &scene.request(url, "pkg.bin", 1024)).expect_err("existing cache file");
    assert!(
        matches!(&error, FetchError::AlreadyCached(name) if name == "pkg.bin"),
        "{error}"
    );
    assert_eq!(fs::read(scene.cache_file("pkg.bin")).expect("cache"), body);
    assert_eq!(scene.audit_text(), before, "the refused fetch was recorded");
}

#[test]
fn task_network_is_required_and_bad_names_are_refused() {
    let scene = Scene::new(&["http://127.0.0.1:9/fixtures/".to_owned()]);
    let mut store = ControlStore::open(&scene.data).expect("store");
    store
        .set_permissions(
            scene.workspace_id,
            PermissionSet::default(),
            store.revision(),
        )
        .expect("disable task_network");
    drop(store);
    let before = scene.audit_text();
    let error = fetch(
        &scene.data,
        &scene.request(
            "http://127.0.0.1:9/fixtures/pkg.bin".to_owned(),
            "pkg.bin",
            1024,
        ),
    )
    .expect_err("task_network disabled");
    assert!(matches!(error, FetchError::Refused(_)), "{error}");
    assert_eq!(scene.audit_text(), before);

    let mut store = ControlStore::open(&scene.data).expect("store");
    store
        .set_permissions(
            scene.workspace_id,
            PermissionSet {
                task_network: true,
                ..PermissionSet::default()
            },
            store.revision(),
        )
        .expect("enable task_network");
    drop(store);
    for name in ["../pkg", "CON", ".hidden", "pkg/child"] {
        let error = fetch(
            &scene.data,
            &scene.request("http://127.0.0.1:9/fixtures/pkg.bin".to_owned(), name, 1024),
        )
        .expect_err("bad cache name");
        assert!(matches!(error, FetchError::Refused(_)), "{name}: {error}");
    }
}
