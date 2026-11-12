//! Development-tool acceptance runs through the trusted CLI.
//!
//! Each case builds, tests, and runs a small real project in a confined
//! workspace, runs a hostile program that tries to read host data, write
//! outside the workspace, and reach a loopback listener, and checks that a
//! missing dependency fails as an ordinary recorded exit.
//!
//! The cases need host toolchains, so they are ignored by default. Run them
//! with `cargo test -p omgcode-cli --test toolchain_acceptance --locked -- --ignored`
//! after setting the variables below. A case whose variable is missing fails;
//! it never passes silently.
//!
//! | Variable | Directory |
//! | --- | --- |
//! | `OMGCODE_ACCEPT_RUST` | a Rust toolchain holding `bin/cargo` and `bin/rustfmt` |
//! | `OMGCODE_ACCEPT_PYTHON` | the directory holding `python.exe`, or `/usr` |
//! | `OMGCODE_ACCEPT_NODE` | the directory holding `node.exe`, or `/usr` |
//! | `OMGCODE_ACCEPT_GO` | a Go root holding `bin/go` |
//! | `OMGCODE_ACCEPT_CC` | Linux only: the root holding `bin/gcc` and `bin/g++` |
//! | `OMGCODE_ACCEPT_MSVC` | Windows only: an MSVC `VC/Tools/MSVC/<version>` directory |
//! | `OMGCODE_ACCEPT_WINSDK` | Windows only: the `Windows Kits/10` directory |
//! | `OMGCODE_ACCEPT_JAVA` | a JDK root holding `bin/javac` |
//!
//! The Windows Rust and C/C++ cases use the MSVC and SDK variables too. On
//! Windows a toolchain root inside the user's profile is granted
//! read-execute for the scene, because application packages cannot read it
//! by default; roots such as `Program Files` are readable by every package.
#![cfg(any(target_os = "linux", windows))]

mod support;

use serde_json::Value;
use std::fs;
use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use support::run_scene::{Scene, run_json, system_path, system_root};
use support::{assert_success, path_arg};

/// The per-run timeout passed to `omgcode run`, in seconds.
const RUN_TIMEOUT: &str = "600";
/// The test's own deadline for one CLI call, above the run timeout.
const CLI_DEADLINE: Duration = Duration::from_secs(660);
const EXE: &str = if cfg!(windows) { ".exe" } else { "" };
const SECRET: &str = "host-data-sentinel-41c7";

/// A toolchain's profile parts.
#[derive(Default)]
struct Toolchain {
    roots: Vec<PathBuf>,
    path_entries: Vec<PathBuf>,
    environment: Vec<(String, String)>,
}

impl Toolchain {
    /// A toolchain root whose `bin` directory, or the root itself, is on PATH.
    fn rooted(root: PathBuf) -> Self {
        let bin = root.join("bin");
        let entry = if bin.is_dir() { bin } else { root.clone() };
        Self {
            roots: vec![root],
            path_entries: vec![entry],
            environment: Vec::new(),
        }
    }

    fn merge(mut self, other: Self) -> Self {
        self.roots.extend(other.roots);
        self.path_entries.extend(other.path_entries);
        self.environment.extend(other.environment);
        self
    }

    fn with(mut self, name: &str, value: impl Into<String>) -> Self {
        self.environment.push((name.to_owned(), value.into()));
        self
    }
}

/// The directory a required acceptance variable names.
fn variable(name: &str) -> PathBuf {
    let value =
        std::env::var_os(name).unwrap_or_else(|| panic!("set {name} to run this acceptance case"));
    let path = PathBuf::from(value);
    assert!(path.is_dir(), "{name} must name a directory");
    path
}

/// Record profile `name` with the system tools plus the toolchain, granting
/// Windows container access to roots that packages cannot read by default.
fn install(scene: &mut Scene, name: &str, toolchain: Toolchain) {
    let mut roots = vec![system_root()];
    for root in toolchain.roots {
        if roots.iter().any(|existing| root.starts_with(existing)) {
            continue;
        }
        if cfg!(windows) && inside_user_profile(&root) {
            scene.grant(&root, "read-execute");
        }
        roots.push(root);
    }
    let mut path_entries = toolchain.path_entries;
    if !path_entries.contains(&system_path()) {
        path_entries.push(system_path());
    }
    assert_success(&scene.add_profile_with(name, &roots, &path_entries, &toolchain.environment));
}

fn inside_user_profile(root: &Path) -> bool {
    std::env::var_os("USERPROFILE").is_some_and(|profile| root.starts_with(profile))
}

/// Write a project file under the workspace.
fn write(scene: &Scene, relative: &str, contents: &str) {
    let path = scene.workspace.join(relative);
    fs::create_dir_all(path.parent().expect("parent")).expect("project directory");
    fs::write(path, contents).expect("project file");
}

fn read(scene: &Scene, relative: &str) -> String {
    fs::read_to_string(scene.workspace.join(relative)).expect("workspace output")
}

/// Run a command in a workspace directory and return its recorded report.
fn run_in(scene: &Scene, profile: &str, cwd: &str, command: &[&str]) -> Value {
    let output = scene.run_for(
        profile,
        &["--cwd", cwd, "--timeout-secs", RUN_TIMEOUT],
        command,
        CLI_DEADLINE,
    );
    run_json(&output)
}

fn text(run: &Value, stream: &str) -> String {
    run[stream]["text"].as_str().unwrap_or_default().to_owned()
}

fn exit_code(run: &Value) -> i64 {
    assert_eq!(run["finish"]["result"]["kind"], "exited", "{run:#}");
    run["finish"]["result"]["code"].as_i64().expect("exit code")
}

/// Run a command that must exit 0, and return its standard output.
fn succeed(scene: &Scene, profile: &str, cwd: &str, command: &[&str]) -> String {
    let run = run_in(scene, profile, cwd, command);
    assert_eq!(
        exit_code(&run),
        0,
        "{command:?} failed\nstdout: {}\nstderr: {}",
        text(&run, "stdout"),
        text(&run, "stderr")
    );
    text(&run, "stdout")
}

/// Run a command that must exit non-zero, and return both output streams.
fn fail(scene: &Scene, profile: &str, cwd: &str, command: &[&str]) -> String {
    let run = run_in(scene, profile, cwd, command);
    assert_ne!(exit_code(&run), 0, "{command:?} unexpectedly succeeded");
    format!("{}\n{}", text(&run, "stdout"), text(&run, "stderr"))
}

/// Host data, a path outside the workspace, and a loopback listener that a
/// confined program must not reach.
struct Hostile {
    secret: PathBuf,
    outside: PathBuf,
    listener: TcpListener,
    port: String,
}

impl Hostile {
    fn new(scene: &Scene) -> Self {
        let secret = scene.path("host-secret.txt");
        fs::write(&secret, SECRET).expect("host secret");
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        listener
            .set_nonblocking(true)
            .expect("non-blocking listener");
        let port = listener
            .local_addr()
            .expect("listener address")
            .port()
            .to_string();
        Self {
            secret,
            outside: scene.path("escaped.txt"),
            listener,
            port,
        }
    }

    /// The secret path, the outside path, and the listener port.
    fn arguments(&self) -> [&str; 3] {
        [path_arg(&self.secret), path_arg(&self.outside), &self.port]
    }

    /// Every attempt was reported denied and nothing reached the host.
    fn assert_denied(&self, stdout: &str, network: bool) {
        assert!(stdout.contains("read=denied"), "{stdout}");
        assert!(stdout.contains("write=denied"), "{stdout}");
        if network {
            assert!(stdout.contains("connect=denied"), "{stdout}");
        }
        assert!(!stdout.contains("allowed"), "{stdout}");
        assert!(!stdout.contains(SECRET), "{stdout}");
        assert!(
            !self.outside.exists(),
            "a confined program wrote outside the workspace"
        );
        match self.listener.accept() {
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            other => panic!("a confined program reached the listener: {other:?}"),
        }
    }
}

/// The MSVC compiler and Windows SDK, with the variables a developer
/// prompt would set. `VCINSTALLDIR` makes rustc use the linker on PATH.
#[cfg(windows)]
fn msvc() -> Toolchain {
    let tools = variable("OMGCODE_ACCEPT_MSVC");
    let kits = variable("OMGCODE_ACCEPT_WINSDK");
    let version = sdk_version(&kits);
    let include = kits.join("Include").join(&version);
    let lib = kits.join("Lib").join(&version);
    let vc = tools.ancestors().nth(3).expect("the VC directory");
    let join = |paths: &[PathBuf]| {
        std::env::join_paths(paths)
            .expect("joinable paths")
            .into_string()
            .expect("UTF-8 paths")
    };
    let include = join(&[
        tools.join("include"),
        include.join("ucrt"),
        include.join("um"),
        include.join("shared"),
    ]);
    let lib = join(&[
        tools.join("lib").join("x64"),
        lib.join("ucrt").join("x64"),
        lib.join("um").join("x64"),
    ]);
    Toolchain {
        roots: vec![tools.clone(), kits],
        path_entries: vec![tools.join("bin").join("Hostx64").join("x64")],
        environment: Vec::new(),
    }
    .with("INCLUDE", include)
    .with("LIB", lib)
    .with("VCINSTALLDIR", format!("{}\\", vc.display()))
}

/// The newest installed SDK version with x64 libraries.
#[cfg(windows)]
fn sdk_version(kits: &Path) -> String {
    fs::read_dir(kits.join("Lib"))
        .expect("SDK libraries")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join("um").join("x64").is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .max_by_key(|name| {
            name.split('.')
                .map(|part| part.parse::<u32>().unwrap_or(0))
                .collect::<Vec<_>>()
        })
        .expect("a Windows SDK with x64 libraries")
}

const RUST_MANIFEST: &str = r#"[package]
name = "accept"
version = "0.1.0"
edition = "2021"

[dependencies]
"#;

// Deliberately unformatted, so `cargo fmt --check` has work to report.
const RUST_LIB: &str = r#"pub fn greeting()->&'static str{"accept-ok"}

#[cfg(test)]
mod tests {
    #[test]
    fn greets() { assert_eq!(super::greeting(), "accept-ok"); }
}
"#;

const RUST_MAIN: &str = r#"use std::net::{SocketAddr, TcpStream};
use std::process::Command;
use std::time::Duration;
use std::{env, fs};

fn report(label: &str, allowed: bool) {
    println!("{label}={}", if allowed { "allowed" } else { "denied" });
}

fn main() {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("child") => println!("child-ok"),
        Some("hostile") => {
            report("read", fs::read(&args[2]).is_ok());
            report("write", fs::write(&args[3], "escaped").is_ok());
            let address: SocketAddr = format!("127.0.0.1:{}", args[4]).parse().expect("address");
            let stream = TcpStream::connect_timeout(&address, Duration::from_secs(2));
            report("connect", stream.is_ok());
        }
        _ => {
            let own = env::current_exe().expect("own path");
            let output = Command::new(own).arg("child").output().expect("child");
            let child = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            fs::create_dir_all("out").expect("output directory");
            fs::write("out/result.txt", &child).expect("result");
            println!("{} {child}", accept::greeting());
        }
    }
}
"#;

const RUST_MISSING_MANIFEST: &str = r#"[package]
name = "missing"
version = "0.1.0"
edition = "2021"

[dependencies]
omgcode-missing-dependency = "1"
"#;

/// Cargo formats, builds, tests, and runs a project; the built program
/// spawns itself, writes only in the workspace, and is denied host access.
#[test]
#[ignore = "needs a Rust toolchain; set OMGCODE_ACCEPT_RUST (and the MSVC variables on Windows)"]
fn rust_projects_build_test_and_run_confined() {
    let mut scene = Scene::new();
    let rust = Toolchain::rooted(variable("OMGCODE_ACCEPT_RUST"));
    #[cfg(windows)]
    let rust = rust.merge(msvc());
    install(&mut scene, "rust", rust);
    write(&scene, "rust/Cargo.toml", RUST_MANIFEST);
    write(&scene, "rust/src/lib.rs", RUST_LIB);
    write(&scene, "rust/src/main.rs", RUST_MAIN);

    let unformatted = fail(&scene, "rust", "rust", &["cargo", "fmt", "--check"]);
    assert!(unformatted.contains("lib.rs"), "{unformatted}");
    succeed(&scene, "rust", "rust", &["cargo", "fmt"]);
    succeed(&scene, "rust", "rust", &["cargo", "fmt", "--check"]);
    succeed(&scene, "rust", "rust", &["cargo", "build", "--offline"]);
    let tests = succeed(&scene, "rust", "rust", &["cargo", "test", "--offline"]);
    assert!(tests.contains("test tests::greets ... ok"), "{tests}");

    let program = format!("rust/target/debug/accept{EXE}");
    let stdout = succeed(&scene, "rust", "rust", &[&program]);
    assert!(stdout.contains("accept-ok child-ok"), "{stdout}");
    assert_eq!(read(&scene, "rust/out/result.txt"), "child-ok");

    let hostile = Hostile::new(&scene);
    let mut command = vec![program.as_str(), "hostile"];
    command.extend(hostile.arguments());
    hostile.assert_denied(&succeed(&scene, "rust", "rust", &command), true);

    write(&scene, "rust-missing/Cargo.toml", RUST_MISSING_MANIFEST);
    write(&scene, "rust-missing/src/main.rs", "fn main() {}\n");
    let missing = fail(
        &scene,
        "rust",
        "rust-missing",
        &["cargo", "build", "--offline"],
    );
    assert!(missing.contains("omgcode-missing-dependency"), "{missing}");
    scene.close();
}

const PY_GREET: &str = "def greeting():\n    return \"accept-ok\"\n";

const PY_APP: &str = r#"import os
import subprocess
import sys

from greet import greeting

child = subprocess.run(
    [sys.executable, "-c", "print('child-ok')"],
    capture_output=True,
    text=True,
    check=True,
).stdout.strip()
os.makedirs("out", exist_ok=True)
with open(os.path.join("out", "result.txt"), "w", encoding="utf-8") as result:
    result.write(child)
print(greeting(), child)
"#;

const PY_TEST: &str = r#"import unittest

from greet import greeting


class GreetingTest(unittest.TestCase):
    def test_greeting(self):
        self.assertEqual(greeting(), "accept-ok")


if __name__ == "__main__":
    unittest.main()
"#;

const PY_HOSTILE: &str = r#"import socket
import sys

secret, outside, port = sys.argv[1], sys.argv[2], int(sys.argv[3])


def attempt(label, action):
    try:
        action()
    except OSError:
        print(f"{label}=denied")
    else:
        print(f"{label}=allowed")


def read():
    with open(secret, encoding="utf-8") as source:
        source.read()


def write():
    with open(outside, "w", encoding="utf-8") as target:
        target.write("escaped")


def connect():
    socket.create_connection(("127.0.0.1", port), timeout=2).close()


attempt("read", read)
attempt("write", write)
attempt("connect", connect)
"#;

/// Python runs scripts, tests, bytecode compilation, and a subprocess in
/// the workspace, and is denied host access.
#[test]
#[ignore = "needs a Python toolchain; set OMGCODE_ACCEPT_PYTHON"]
fn python_scripts_tests_and_subprocesses_run_confined() {
    let mut scene = Scene::new();
    install(
        &mut scene,
        "python",
        Toolchain::rooted(variable("OMGCODE_ACCEPT_PYTHON")),
    );
    let python = if cfg!(windows) { "python" } else { "python3" };
    write(&scene, "py/greet.py", PY_GREET);
    write(&scene, "py/app.py", PY_APP);
    write(&scene, "py/test_app.py", PY_TEST);
    write(&scene, "py/hostile.py", PY_HOSTILE);

    let stdout = succeed(&scene, "python", "py", &[python, "app.py"]);
    assert!(stdout.contains("accept-ok child-ok"), "{stdout}");
    assert_eq!(read(&scene, "py/out/result.txt"), "child-ok");
    let run = run_in(&scene, "python", "py", &[python, "-m", "unittest", "-v"]);
    assert_eq!(exit_code(&run), 0, "{run:#}");
    assert!(text(&run, "stderr").contains("OK"), "{run:#}");
    succeed(
        &scene,
        "python",
        "py",
        &[python, "-m", "compileall", "-q", "."],
    );
    assert!(scene.workspace.join("py/__pycache__").is_dir());

    let hostile = Hostile::new(&scene);
    let mut command = vec![python, "hostile.py"];
    command.extend(hostile.arguments());
    hostile.assert_denied(&succeed(&scene, "python", "py", &command), true);

    let missing = fail(
        &scene,
        "python",
        "py",
        &[python, "-c", "import omgcode_missing_dependency"],
    );
    assert!(missing.contains("ModuleNotFoundError"), "{missing}");
    scene.close();
}

const NODE_GREET: &str = "exports.greeting = () => 'accept-ok';\n";

// The child inherits stdio and reports through a workspace file, because
// piped child stdio hangs in a Windows AppContainer (see docs/execution.md).
const NODE_APP: &str = r#"const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const { greeting } = require('./greet');

fs.mkdirSync('out', { recursive: true });
const child = spawnSync(
  process.execPath,
  ['-e', "require('node:fs').writeFileSync('out/child.txt', 'child-ok')"],
  { stdio: 'inherit' },
);
if (child.status !== 0) {
  throw new Error(`child failed: ${child.status} ${child.error}`);
}
const result = fs.readFileSync('out/child.txt', 'utf8');
fs.writeFileSync('out/result.txt', result);
console.log(greeting(), result);
"#;

const NODE_TEST: &str = r#"const test = require('node:test');
const assert = require('node:assert');
const { greeting } = require('./greet');

test('greeting', () => {
  assert.strictEqual(greeting(), 'accept-ok');
});
"#;

const NODE_HOSTILE: &str = r#"const fs = require('node:fs');
const net = require('node:net');

const [secret, outside, port] = process.argv.slice(2);
function attempt(label, action) {
  try {
    action();
    console.log(`${label}=allowed`);
  } catch {
    console.log(`${label}=denied`);
  }
}
attempt('read', () => fs.readFileSync(secret, 'utf8'));
attempt('write', () => fs.writeFileSync(outside, 'escaped'));

let reported = false;
let socket = null;
function connected(allowed) {
  if (!reported) {
    reported = true;
    console.log(`connect=${allowed ? 'allowed' : 'denied'}`);
    if (socket) {
      socket.destroy();
    }
    clearTimeout(timer);
  }
}
const timer = setTimeout(() => connected(false), 2000);
try {
  socket = net.connect({ host: '127.0.0.1', port: Number(port) });
  socket.on('connect', () => connected(true));
  socket.on('error', () => connected(false));
} catch {
  connected(false);
}
"#;

/// Node runs scripts, its test runner, and a child process in the
/// workspace, and is denied host access.
#[test]
#[ignore = "needs a Node.js toolchain; set OMGCODE_ACCEPT_NODE"]
fn node_scripts_tests_and_children_run_confined() {
    let mut scene = Scene::new();
    let node = Toolchain::rooted(variable("OMGCODE_ACCEPT_NODE"));
    // Node's module loader resolves real paths by reading the attributes of
    // every ancestor directory, starting at the drive root, which an
    // AppContainer cannot read. Preserving symbolic links skips that walk.
    #[cfg(windows)]
    let node = node.with(
        "NODE_OPTIONS",
        "--preserve-symlinks --preserve-symlinks-main",
    );
    // Distribution builds read the system OpenSSL configuration, which
    // `/usr/lib/ssl` links to under `/etc/ssl`.
    #[cfg(target_os = "linux")]
    let node = if Path::new("/etc/ssl").is_dir() {
        node.merge(Toolchain {
            roots: vec![PathBuf::from("/etc/ssl")],
            ..Toolchain::default()
        })
    } else {
        node
    };
    install(&mut scene, "node", node);
    write(&scene, "js/greet.js", NODE_GREET);
    write(&scene, "js/app.js", NODE_APP);
    write(&scene, "js/app.test.js", NODE_TEST);
    write(&scene, "js/hostile.js", NODE_HOSTILE);

    let stdout = succeed(&scene, "node", "js", &["node", "app.js"]);
    assert!(stdout.contains("accept-ok child-ok"), "{stdout}");
    assert_eq!(read(&scene, "js/out/result.txt"), "child-ok");
    // On Windows the test runner must not isolate files in child processes,
    // which need piped stdio; `--test-isolation` needs Node 22.8 or later.
    let test_command: &[&str] = if cfg!(windows) {
        &["node", "--test", "--test-isolation=none"]
    } else {
        &["node", "--test"]
    };
    let tests = succeed(&scene, "node", "js", test_command);
    assert!(tests.contains("pass 1"), "{tests}");

    let hostile = Hostile::new(&scene);
    let mut command = vec!["node", "hostile.js"];
    command.extend(hostile.arguments());
    hostile.assert_denied(&succeed(&scene, "node", "js", &command), true);

    let missing = fail(
        &scene,
        "node",
        "js",
        &["node", "-e", "require('omgcode-missing-dependency')"],
    );
    assert!(missing.contains("Cannot find module"), "{missing}");
    scene.close();
}

const GO_MOD: &str = "module example.com/accept\n\ngo 1.21\n";

// Deliberately unformatted, so `gofmt -l` has a file to report.
const GO_MAIN: &str = r#"package main

import (
    "fmt"
    "net"
    "os"
    "os/exec"
    "strings"
    "time"
)

func greeting()  string { return "accept-ok" }

func report(label string, err error) {
    if err == nil {
        fmt.Printf("%s=allowed\n", label)
    } else {
        fmt.Printf("%s=denied\n", label)
    }
}

func main() {
    if len(os.Args) > 1 && os.Args[1] == "child" {
        fmt.Println("child-ok")
        return
    }
    if len(os.Args) > 4 && os.Args[1] == "hostile" {
        _, err := os.ReadFile(os.Args[2])
        report("read", err)
        report("write", os.WriteFile(os.Args[3], []byte("escaped"), 0o600))
        conn, err := net.DialTimeout("tcp", "127.0.0.1:"+os.Args[4], 2*time.Second)
        if err == nil {
            conn.Close()
        }
        report("connect", err)
        return
    }
    self, err := os.Executable()
    if err != nil {
        panic(err)
    }
    output, err := exec.Command(self, "child").Output()
    if err != nil {
        panic(err)
    }
    child := strings.TrimSpace(string(output))
    if err := os.MkdirAll("out", 0o755); err != nil {
        panic(err)
    }
    if err := os.WriteFile("out/result.txt", []byte(child), 0o644); err != nil {
        panic(err)
    }
    fmt.Println(greeting(), child)
}
"#;

const GO_TEST: &str = r#"package main

import "testing"

func TestGreeting(t *testing.T) {
	if greeting() != "accept-ok" {
		t.Fatal("unexpected greeting")
	}
}
"#;

const GO_MISSING: &str = r#"package main

import "example.com/omgcode/missing"

func main() { missing.Run() }
"#;

/// Go formats, builds, tests, and runs a module with its caches in the
/// workspace and the module proxy off, and is denied host access.
#[test]
#[ignore = "needs a Go toolchain; set OMGCODE_ACCEPT_GO"]
fn go_modules_build_test_and_run_confined() {
    let mut scene = Scene::new();
    let cache = scene.workspace.join(".go");
    let go = Toolchain::rooted(variable("OMGCODE_ACCEPT_GO"))
        .with("GOCACHE", cache.join("cache").display().to_string())
        .with("GOPATH", cache.join("path").display().to_string())
        .with("GOENV", "off")
        .with("GOTOOLCHAIN", "local")
        .with("GOPROXY", "off")
        .with("GOFLAGS", "-buildvcs=false")
        .with("CGO_ENABLED", "0");
    install(&mut scene, "go", go);
    write(&scene, "go/go.mod", GO_MOD);
    write(&scene, "go/main.go", GO_MAIN);
    write(&scene, "go/main_test.go", GO_TEST);

    let unformatted = succeed(&scene, "go", "go", &["gofmt", "-l", "."]);
    assert!(unformatted.contains("main.go"), "{unformatted}");
    succeed(&scene, "go", "go", &["gofmt", "-w", "."]);
    let formatted = succeed(&scene, "go", "go", &["gofmt", "-l", "."]);
    assert!(formatted.trim().is_empty(), "{formatted}");
    let binary = format!("out/accept{EXE}");
    succeed(&scene, "go", "go", &["go", "build", "-o", &binary, "."]);
    let tests = succeed(&scene, "go", "go", &["go", "test", "./..."]);
    assert!(tests.contains("ok"), "{tests}");

    let program = format!("go/{binary}");
    let stdout = succeed(&scene, "go", "go", &[&program]);
    assert!(stdout.contains("accept-ok child-ok"), "{stdout}");
    assert_eq!(read(&scene, "go/out/result.txt"), "child-ok");

    let hostile = Hostile::new(&scene);
    let mut command = vec![program.as_str(), "hostile"];
    command.extend(hostile.arguments());
    hostile.assert_denied(&succeed(&scene, "go", "go", &command), true);

    write(
        &scene,
        "go-missing/go.mod",
        "module example.com/missing\n\ngo 1.21\n",
    );
    write(&scene, "go-missing/main.go", GO_MISSING);
    let missing = fail(&scene, "go", "go-missing", &["go", "build", "./..."]);
    assert!(missing.contains("example.com/omgcode/missing"), "{missing}");
    scene.close();
}

const C_HEADER: &str = "const char *greeting(void);\n";

const C_GREET: &str =
    "#include \"greet.h\"\n\nconst char *greeting(void) { return \"accept-ok\"; }\n";

const C_APP: &str = r#"#define _CRT_SECURE_NO_WARNINGS
#include <stdio.h>
#include <string.h>

#include "greet.h"

static void report(const char *label, int allowed) {
    printf("%s=%s\n", label, allowed ? "allowed" : "denied");
}

int main(int argc, char **argv) {
    if (argc >= 4 && strcmp(argv[1], "hostile") == 0) {
        FILE *secret = fopen(argv[2], "rb");
        report("read", secret != NULL);
        if (secret != NULL) {
            fclose(secret);
        }
        FILE *outside = fopen(argv[3], "wb");
        report("write", outside != NULL);
        if (outside != NULL) {
            fclose(outside);
        }
        return 0;
    }
    FILE *result = fopen("result.txt", "wb");
    if (result == NULL) {
        return 1;
    }
    fputs(greeting(), result);
    fclose(result);
    printf("%s\n", greeting());
    return 0;
}
"#;

const C_CHECK: &str = r#"#include <string.h>

#include "greet.h"

int main(void) { return strcmp(greeting(), "accept-ok") == 0 ? 0 : 1; }
"#;

const CPP_APP: &str = r#"#include <fstream>
#include <iostream>
#include <string>
#include <vector>

int main() {
    std::vector<std::string> parts{"accept", "cpp"};
    std::string joined = parts[0] + "-" + parts[1];
    std::ofstream result("result-cpp.txt");
    result << joined;
    std::cout << joined << "\n";
    return 0;
}
"#;

const C_MISSING: &str = "#include \"omgcode_missing.h\"\n\nint main(void) { return 0; }\n";

#[cfg(target_os = "linux")]
fn c_toolchain() -> Toolchain {
    Toolchain::rooted(variable("OMGCODE_ACCEPT_CC"))
}

#[cfg(windows)]
fn c_toolchain() -> Toolchain {
    msvc()
}

#[cfg(target_os = "linux")]
const C_BUILDS: [&[&str]; 3] = [
    &[
        "gcc", "-std=c11", "-Wall", "-Wextra", "-Werror", "-o", "app", "app.c", "greet.c",
    ],
    &[
        "gcc", "-std=c11", "-Wall", "-Wextra", "-Werror", "-o", "check", "check.c", "greet.c",
    ],
    &[
        "g++",
        "-std=c++17",
        "-Wall",
        "-Wextra",
        "-Werror",
        "-o",
        "app_cpp",
        "app.cpp",
    ],
];

#[cfg(windows)]
const C_BUILDS: [&[&str]; 3] = [
    &[
        "cl",
        "/nologo",
        "/W4",
        "/WX",
        "app.c",
        "greet.c",
        "/Fe:app.exe",
    ],
    &[
        "cl",
        "/nologo",
        "/W4",
        "/WX",
        "check.c",
        "greet.c",
        "/Fe:check.exe",
    ],
    &[
        "cl",
        "/nologo",
        "/W4",
        "/WX",
        "/EHsc",
        "/std:c++17",
        "app.cpp",
        "/Fe:app_cpp.exe",
    ],
];

#[cfg(target_os = "linux")]
const C_MISSING_BUILD: &[&str] = &["gcc", "-c", "missing.c"];

#[cfg(windows)]
const C_MISSING_BUILD: &[&str] = &["cl", "/nologo", "/c", "missing.c"];

/// The C and C++ compilers build and check programs whose outputs stay in
/// the workspace; a compiled program is denied host file access.
#[test]
#[ignore = "needs a C/C++ toolchain; set OMGCODE_ACCEPT_CC on Linux or the MSVC variables on Windows"]
fn c_and_cpp_programs_build_and_run_confined() {
    let mut scene = Scene::new();
    install(&mut scene, "cc", c_toolchain());
    write(&scene, "c/greet.h", C_HEADER);
    write(&scene, "c/greet.c", C_GREET);
    write(&scene, "c/app.c", C_APP);
    write(&scene, "c/check.c", C_CHECK);
    write(&scene, "c/app.cpp", CPP_APP);
    write(&scene, "c/missing.c", C_MISSING);

    for build in C_BUILDS {
        succeed(&scene, "cc", "c", build);
    }
    succeed(&scene, "cc", "c", &[&format!("c/check{EXE}")]);
    let app = format!("c/app{EXE}");
    let stdout = succeed(&scene, "cc", "c", &[&app]);
    assert!(stdout.contains("accept-ok"), "{stdout}");
    assert_eq!(read(&scene, "c/result.txt"), "accept-ok");
    let stdout = succeed(&scene, "cc", "c", &[&format!("c/app_cpp{EXE}")]);
    assert!(stdout.contains("accept-cpp"), "{stdout}");
    assert_eq!(read(&scene, "c/result-cpp.txt"), "accept-cpp");

    let hostile = Hostile::new(&scene);
    let mut command = vec![app.as_str(), "hostile"];
    command.extend(hostile.arguments());
    hostile.assert_denied(&succeed(&scene, "cc", "c", &command), false);

    let missing = fail(&scene, "cc", "c", C_MISSING_BUILD);
    assert!(missing.contains("omgcode_missing.h"), "{missing}");
    scene.close();
}

const JAVA_MAIN: &str = r#"import java.io.IOException;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;

public class Main {
    static String greeting() {
        return "accept-ok";
    }

    static void report(String label, boolean allowed) {
        System.out.println(label + "=" + (allowed ? "allowed" : "denied"));
    }

    public static void main(String[] args) throws Exception {
        if (args.length == 1 && args[0].equals("child")) {
            System.out.println("child-ok");
            return;
        }
        if (args.length == 4 && args[0].equals("hostile")) {
            boolean read = true;
            try {
                Files.readAllBytes(Path.of(args[1]));
            } catch (IOException | SecurityException e) {
                read = false;
            }
            report("read", read);
            boolean write = true;
            try {
                Files.writeString(Path.of(args[2]), "escaped");
            } catch (IOException | SecurityException e) {
                write = false;
            }
            report("write", write);
            boolean connect = true;
            try (Socket socket = new Socket()) {
                socket.connect(new InetSocketAddress("127.0.0.1", Integer.parseInt(args[3])), 2000);
            } catch (IOException | SecurityException e) {
                connect = false;
            }
            report("connect", connect);
            return;
        }
        Path java = Path.of(System.getProperty("java.home"), "bin", "java");
        Process child = new ProcessBuilder(
                java.toString(), "-XX:-UsePerfData",
                "-cp", System.getProperty("java.class.path"), "Main", "child")
            .redirectErrorStream(true)
            .start();
        String output = new String(child.getInputStream().readAllBytes(), StandardCharsets.UTF_8).trim();
        if (child.waitFor() != 0) {
            throw new IllegalStateException("child failed: " + output);
        }
        Files.createDirectories(Path.of("out"));
        Files.writeString(Path.of("out", "result.txt"), output);
        System.out.println(greeting() + " " + output);
    }
}
"#;

const JAVA_CHECK: &str = r#"public class Check {
    public static void main(String[] args) {
        System.exit(Main.greeting().equals("accept-ok") ? 0 : 1);
    }
}
"#;

const JAVA_MISSING: &str = r#"import org.omgcode.missing.Thing;

public class Missing {
    public static void main(String[] args) {
        Thing.run();
    }
}
"#;

/// The JDK compiles and runs classes and a child JVM in the workspace, and
/// is denied host access. `-XX:-UsePerfData` keeps the JVM from writing
/// performance data into a shared temporary directory.
#[test]
#[ignore = "needs a JDK; set OMGCODE_ACCEPT_JAVA"]
fn java_programs_compile_and_run_confined() {
    let mut scene = Scene::new();
    install(
        &mut scene,
        "java",
        Toolchain::rooted(variable("OMGCODE_ACCEPT_JAVA")),
    );
    write(&scene, "java/Main.java", JAVA_MAIN);
    write(&scene, "java/Check.java", JAVA_CHECK);
    write(&scene, "java-missing/Missing.java", JAVA_MISSING);

    succeed(
        &scene,
        "java",
        "java",
        &[
            "javac",
            "-J-XX:-UsePerfData",
            "-d",
            "classes",
            "Main.java",
            "Check.java",
        ],
    );
    let jvm = ["java", "-XX:-UsePerfData", "-cp", "classes"];
    succeed(&scene, "java", "java", &[&jvm[..], &["Check"]].concat());
    let stdout = succeed(&scene, "java", "java", &[&jvm[..], &["Main"]].concat());
    assert!(stdout.contains("accept-ok child-ok"), "{stdout}");
    assert_eq!(read(&scene, "java/out/result.txt"), "child-ok");

    let hostile = Hostile::new(&scene);
    let mut command = [&jvm[..], &["Main", "hostile"]].concat();
    command.extend(hostile.arguments());
    hostile.assert_denied(&succeed(&scene, "java", "java", &command), true);

    let missing = fail(
        &scene,
        "java",
        "java-missing",
        &[
            "javac",
            "-J-XX:-UsePerfData",
            "-d",
            "classes",
            "Missing.java",
        ],
    );
    assert!(missing.contains("org.omgcode.missing"), "{missing}");
    scene.close();
}
