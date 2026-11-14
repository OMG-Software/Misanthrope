//! Trusted package fetch through the real CLI with a local registry fixture.
#![cfg(any(target_os = "linux", windows))]

mod support;

use std::fs;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use support::{assert_error_code, assert_success, json_output, path_arg, run_cli};
use tempfile::TempDir;

const PROFILE: &str = "registries";
const BODY: &[u8] = b"fetched-package-bytes";

struct Fixture {
    address: SocketAddr,
    task: JoinHandle<String>,
}

impl Fixture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        listener
            .set_nonblocking(true)
            .expect("make fixture accept bounded");
        let address = listener.local_addr().expect("fixture address");
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
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                BODY.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response headers");
            stream.write_all(BODY).expect("write response body");
            head
        });
        Self { address, task }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    fn finish(self) -> String {
        let deadline = Instant::now() + Duration::from_secs(8);
        while !self.task.is_finished() {
            assert!(Instant::now() < deadline, "fixture finished");
            thread::sleep(Duration::from_millis(2));
        }
        self.task.join().expect("fixture joined")
    }
}

fn read_head(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 1024];
    while bytes.len() < 16 * 1024 {
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

fn revision(data_root: &Path) -> u64 {
    let output = run_cli(data_root, &["workspace", "status"]);
    assert_success(&output);
    json_output(&output)["revision"].as_u64().expect("revision")
}

#[test]
fn a_trusted_fetch_writes_a_private_cache_and_is_audited() {
    // Given a registered workspace with task_network and a declared loopback
    // registry fixture.
    let fixture = Fixture::start();
    let temporary = TempDir::new().expect("temporary directory");
    let data_root = temporary.path().join("data");
    let development_root = temporary.path().join("dev");
    let workspace = development_root.join("work");
    fs::create_dir_all(&workspace).expect("workspace");
    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "set-root",
            path_arg(&development_root),
            "--revision",
            "0",
        ],
    );
    assert_success(&output);
    let initial_revision = json_output(&output)["revision"].as_u64().expect("revision");
    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "register",
            path_arg(&workspace),
            "--revision",
            &initial_revision.to_string(),
        ],
    );
    assert_success(&output);
    let id = json_output(&output)["registration_id"]
        .as_str()
        .expect("workspace id")
        .to_owned();
    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "permissions",
            &id,
            "--task-network",
            "true",
            "--revision",
            &revision(&data_root).to_string(),
        ],
    );
    assert_success(&output);
    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "package-profile-add",
            PROFILE,
            "--registry",
            &format!("http://{}/fixtures/", fixture.address),
            "--revision",
            &revision(&data_root).to_string(),
        ],
    );
    assert_success(&output);

    // When the operator fetches a declared package URL.
    let url = fixture.url("/fixtures/pkg.bin");
    let output = run_cli(
        &data_root,
        &[
            "packages",
            "fetch",
            &id,
            "--profile",
            PROFILE,
            "--url",
            &url,
            "--name",
            "pkg.bin",
        ],
    );
    assert_success(&output);

    // Then the report and private cache match the response, and the audit
    // records the fetch without the URL text.
    let report = json_output(&output)["fetch"].clone();
    assert_eq!(report["bytes"], BODY.len());
    assert_eq!(report["status"], 200);
    assert_eq!(report["profile_name"], PROFILE);
    let cache = data_root.join("package-cache").join(&id).join("pkg.bin");
    assert_eq!(fs::read(&cache).expect("cache file"), BODY);
    let head = fixture.finish();
    assert!(head.starts_with("GET /fixtures/pkg.bin "), "{head}");
    let audit = run_cli(&data_root, &["workspace", "audit", "--limit", "100"]);
    assert_success(&audit);
    let audit = serde_json::to_string(&json_output(&audit)).expect("audit JSON");
    assert!(audit.contains("package_fetch_started"), "{audit}");
    assert!(audit.contains("package_fetch_finished"), "{audit}");
    assert!(
        !audit.contains(&url),
        "the request URL was recorded: {audit}"
    );

    // Then invalid bounds are configuration errors and an unknown profile is
    // a package error; neither reaches the network.
    for options in [
        ["--max-bytes", "0"],
        ["--max-bytes", "1073741825"],
        ["--timeout-secs", "0"],
        ["--timeout-secs", "601"],
    ] {
        let output = run_cli(
            &data_root,
            &[
                "packages",
                "fetch",
                &id,
                "--profile",
                PROFILE,
                "--url",
                &url,
                "--name",
                "other.bin",
                options[0],
                options[1],
            ],
        );
        assert_error_code(&output, "configuration");
    }
    let output = run_cli(
        &data_root,
        &[
            "packages",
            "fetch",
            &id,
            "--profile",
            "missing",
            "--url",
            &url,
            "--name",
            "other.bin",
        ],
    );
    assert_error_code(&output, "packages");
}
