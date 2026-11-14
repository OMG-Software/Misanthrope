//! Real CLI tests for chat with the persisted model read/list tool preference.

mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use support::run_scene::{Scene, run_command, shell};
use support::workspace_fixture::Fixture;
use support::{
    CLI_DEADLINE, HANG_FIXTURE_ENV, assert_error_code, assert_success, json_output, path_arg,
    run_cli, run_with_deadline,
};

const NOTE: &[u8] = b"workspace notes for the model";
const IN_ROOT: &str = "sentinel-in-development-root";
const BEYOND_ROOT: &str = "sentinel-beyond-development-root";
const MOCK_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_HTTP_BYTES: usize = 1024 * 1024;

/// Serve a fixed sequence of responses, one per connection, and keep each request.
struct SequenceServer {
    endpoint: String,
    requests: Receiver<Value>,
    worker: JoinHandle<Result<usize, String>>,
}

impl SequenceServer {
    fn start(responses: Vec<Value>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind private mock server");
        listener.set_nonblocking(true).expect("bounded accept mode");
        let address = listener.local_addr().expect("mock server address");
        let (sender, requests) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut served = 0;
            for response in responses {
                // The CLI stops early after a refused call; that is not an error.
                let Ok(mut stream) = accept(&listener) else {
                    break;
                };
                let body = read_body(&mut stream)?;
                let request: Value = serde_json::from_slice(&body)
                    .map_err(|error| format!("request JSON: {error}"))?;
                sender.send(request).map_err(|error| error.to_string())?;
                let bytes = serde_json::to_vec(&response).map_err(|error| error.to_string())?;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                );
                stream
                    .write_all(head.as_bytes())
                    .and_then(|()| stream.write_all(&bytes))
                    .map_err(|error| error.to_string())?;
                served += 1;
            }
            Ok(served)
        });
        Self {
            endpoint: format!("http://{address}/v1"),
            requests,
            worker,
        }
    }

    /// Return every captured request after the server thread has finished.
    fn finish(self) -> Vec<Value> {
        let served = self
            .worker
            .join()
            .expect("mock server thread")
            .expect("mock server exchange");
        let requests: Vec<Value> = self.requests.try_iter().collect();
        assert_eq!(requests.len(), served);
        requests
    }
}

fn accept(listener: &TcpListener) -> Result<TcpStream, String> {
    let deadline = Instant::now() + MOCK_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .map_err(|error| error.to_string())?;
                stream
                    .set_read_timeout(Some(MOCK_TIMEOUT))
                    .map_err(|error| error.to_string())?;
                return Ok(stream);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err("no further request".to_owned());
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn read_body(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            let length: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .ok_or("missing content length")?
                .trim()
                .parse()
                .map_err(|_| "invalid content length")?;
            if length > MAX_HTTP_BYTES {
                return Err("request too large".to_owned());
            }
            while bytes.len() < end + 4 + length {
                let read = stream
                    .read(&mut buffer)
                    .map_err(|error| error.to_string())?;
                if read == 0 {
                    return Err("truncated body".to_owned());
                }
                bytes.extend_from_slice(&buffer[..read]);
            }
            return Ok(bytes[end + 4..end + 4 + length].to_vec());
        }
        if bytes.len() > MAX_HTTP_BYTES {
            return Err("headers too large".to_owned());
        }
        let read = stream
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            return Err("connection closed before headers".to_owned());
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
}

fn text(content: &str) -> Value {
    json!({
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }]
    })
}

fn tool_call(id: &str, name: &str, arguments: &Value) -> Value {
    json!({
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments.to_string()}
                }]
            },
            "finish_reason": "tool_calls"
        }]
    })
}

impl Fixture {
    /// Create a session for the registered workspace through the CLI.
    fn session(&self) -> PathBuf {
        let output = run_cli(
            &self.data,
            &["session", "new", "--workspace", path_arg(&self.workspace)],
        );
        assert_success(&output);
        PathBuf::from(json_output(&output)["journal"].as_str().expect("journal"))
    }

    fn revision(&self) -> String {
        let output = run_cli(&self.data, &["workspace", "status"]);
        assert_success(&output);
        json_output(&output)["revision"].to_string()
    }

    fn set(&self, flag: &str, value: bool) {
        let revision = self.revision();
        let output = run_cli(
            &self.data,
            &[
                "workspace",
                "permissions",
                &self.workspace_id.to_string(),
                "--revision",
                &revision,
                flag,
                if value { "true" } else { "false" },
            ],
        );
        assert_success(&output);
    }

    fn chat(&self, journal: &Path, endpoint: &str, message: &str) -> Output {
        run_cli(
            &self.data,
            &[
                "chat",
                "--journal",
                path_arg(journal),
                "--endpoint",
                endpoint,
                "--model",
                "fixture",
                "--context-tokens",
                "131072",
                "--max-output-tokens",
                "64",
                message,
            ],
        )
    }

    fn root(&self) -> PathBuf {
        self.workspace
            .parent()
            .expect("development root")
            .to_path_buf()
    }
}

fn user_texts(request: &Value) -> Vec<String> {
    request["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .filter(|message| message["role"] == "user")
        .map(|message| message["content"].as_str().expect("text").to_owned())
        .collect()
}

#[test]
fn a_disabled_preference_keeps_chat_text_only_and_settings_bytes_compatible() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    let status = run_cli(&fixture.data, &["workspace", "status"]);
    assert_success(&status);
    assert_eq!(
        json_output(&status)["workspaces"][0]["permissions"]["model_tools"],
        false,
        "status must report the disabled preference explicitly"
    );
    let settings = fs::read_to_string(fixture.data.join("control-v1.jsonl")).expect("settings");
    assert!(
        !settings.contains("model_tools"),
        "a disabled preference must not change persisted settings bytes"
    );

    let server = SequenceServer::start(vec![text("text only")]);
    let output = fixture.chat(&journal, &server.endpoint, "read note.txt");
    let requests = server.finish();

    assert_success(&output);
    let result = json_output(&output);
    assert_eq!(result["reply"]["content"], "text only");
    assert_eq!(result["model_tools"]["mode"], "disabled");
    assert_eq!(result["model_tools"]["reason"], "preference_disabled");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].get("tools").is_none(), "no tools were offered");
}

#[test]
fn an_enabled_preference_lets_the_model_read_the_workspace_through_the_runtime() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    let settings = fs::read_to_string(fixture.data.join("control-v1.jsonl")).expect("settings");
    assert!(settings.contains("\"model_tools\":true"));

    let server = SequenceServer::start(vec![
        tool_call(
            "call-1",
            "workspace_read_file",
            &json!({"path": "note.txt"}),
        ),
        text("The note says hello."),
    ]);
    let output = fixture.chat(&journal, &server.endpoint, "what does note.txt say?");
    let requests = server.finish();

    assert_success(&output);
    let result = json_output(&output);
    assert_eq!(result["reply"]["content"], "The note says hello.");
    assert_eq!(result["model_tools"]["mode"], "enabled");
    assert_eq!(
        result["model_tools"]["workspace_id"],
        fixture.workspace_id.to_string()
    );
    assert_eq!(result["model_tools"]["tool_calls"], 1);
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request["tool_choice"], "auto");
        let names: Vec<&str> = request["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|tool| tool["function"]["name"].as_str().expect("name"))
            .collect();
        assert_eq!(names, ["workspace_read_file", "workspace_list_directory"]);
        // The compiled base prompt remains the only system message.
        let systems = request["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .filter(|message| message["role"] == "system")
            .count();
        assert_eq!(systems, 1);
    }
    let tool_text = user_texts(&requests[1])
        .into_iter()
        .find(|text| text.starts_with("Tool result [call call-1"))
        .expect("source-labelled tool result sent as user content");
    assert!(tool_text.contains("untrusted workspace data"));
    assert!(tool_text.contains(std::str::from_utf8(NOTE).expect("UTF-8 note")));

    let events = fs::read_to_string(&journal).expect("journal");
    assert!(events.contains("\"tool.requested\""));
    assert!(events.contains("\"tool.completed\""));
}

#[test]
fn the_server_token_in_a_workspace_file_is_redacted_before_the_journal_and_model() {
    const TOKEN: &str = "fixture-lm-credential-654321";
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    fs::write(
        fixture.workspace.join("secret.txt"),
        format!(
            "server token {TOKEN}
"
        ),
    )
    .expect("write file with the token");
    let server = SequenceServer::start(vec![
        tool_call(
            "call-1",
            "workspace_read_file",
            &json!({"path": "secret.txt"}),
        ),
        text("Read it."),
    ]);

    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_omgcode"));
    command
        .env("OMGCODE_LM_TOKEN", TOKEN)
        .env_remove(HANG_FIXTURE_ENV)
        .arg("--data-dir")
        .arg(&fixture.data)
        .args(["chat", "--journal", path_arg(&journal)])
        .args(["--endpoint", &server.endpoint, "--model", "fixture"])
        .args(["--context-tokens", "131072", "--max-output-tokens", "64"])
        .arg("read secret.txt");
    let result = run_with_deadline(command, CLI_DEADLINE).expect("run OMGCode CLI");
    assert!(!result.timed_out, "CLI exceeded its deadline");
    let output = result.output;
    let requests = server.finish();

    assert_success(&output);
    assert_eq!(json_output(&output)["model_tools"]["tool_calls"], 1);
    assert!(!String::from_utf8_lossy(&output.stdout).contains(TOKEN));
    let tool_text = user_texts(&requests[1])
        .into_iter()
        .find(|text| text.starts_with("Tool result [call call-1"))
        .expect("tool result sent to the model");
    assert!(tool_text.contains("server token [REDACTED]"));
    for request in &requests {
        assert!(!request.to_string().contains(TOKEN));
    }
    let events = fs::read_to_string(&journal).expect("journal");
    assert!(events.contains("server token [REDACTED]"));
    assert!(!events.contains(TOKEN));
}

/// Run chat with a configured LM Studio token against `server`.
fn chat_with_token(fixture: &Fixture, journal: &Path, endpoint: &str, token: &str) -> Output {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_omgcode"));
    command
        .env("OMGCODE_LM_TOKEN", token)
        .env_remove(HANG_FIXTURE_ENV)
        .arg("--data-dir")
        .arg(&fixture.data)
        .args(["chat", "--journal", path_arg(journal)])
        .args(["--endpoint", endpoint, "--model", "fixture"])
        .args(["--context-tokens", "131072", "--max-output-tokens", "64"])
        .arg("read a file");
    let result = run_with_deadline(command, CLI_DEADLINE).expect("run OMGCode CLI");
    assert!(!result.timed_out, "CLI exceeded its deadline");
    result.output
}

#[test]
fn a_tool_call_carrying_the_server_token_is_refused_before_the_journal() {
    const TOKEN: &str = "fixture-lm-credential-987654";
    // A backslash is escaped inside the JSON-encoded arguments string, so only
    // a search of the decoded arguments finds this token.
    const ESCAPED_TOKEN: &str = r"fixture\lm-credential-246810";
    let in_arguments = tool_call(
        "call-1",
        "workspace_read_file",
        &json!({"path": format!("{TOKEN}.txt")}),
    );
    let in_id = tool_call(
        &format!("call-{TOKEN}"),
        "workspace_read_file",
        &json!({"path": "note.txt"}),
    );
    let in_encoded_arguments = tool_call(
        "call-1",
        "workspace_read_file",
        &json!({"path": format!("{ESCAPED_TOKEN}.txt")}),
    );
    for (token, reply) in [
        (TOKEN, in_arguments),
        (TOKEN, in_id),
        (ESCAPED_TOKEN, in_encoded_arguments),
    ] {
        let fixture = Fixture::new(NOTE);
        let journal = fixture.session();
        fixture.set("--model-tools", true);
        let server = SequenceServer::start(vec![reply]);
        let output = chat_with_token(&fixture, &journal, &server.endpoint, token);
        let requests = server.finish();

        assert_error_code(&output, "provider");
        assert_eq!(requests.len(), 1, "a refused call ends the turn");
        assert!(!String::from_utf8_lossy(&output.stdout).contains(token));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(token));
        let events = fs::read_to_string(&journal).expect("journal");
        assert!(!events.contains("lm-credential"));
        assert!(!events.contains("\"tool.requested\""));
        assert!(events.contains("turn.failed"));
    }
}

#[test]
fn text_chat_withholds_saved_tool_results_after_model_tools_are_disabled() {
    let note = std::str::from_utf8(NOTE).expect("UTF-8 note");
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    let server = SequenceServer::start(vec![
        tool_call(
            "call-1",
            "workspace_read_file",
            &json!({"path": "note.txt"}),
        ),
        text("Read it."),
    ]);
    let output = fixture.chat(&journal, &server.endpoint, "read note.txt");
    let requests = server.finish();
    assert_success(&output);
    assert!(requests[1].to_string().contains(note));

    fixture.set("--model-tools", false);
    let server = SequenceServer::start(vec![text("No file access now.")]);
    let output = fixture.chat(&journal, &server.endpoint, "what did it say?");
    let requests = server.finish();

    assert_success(&output);
    let result = json_output(&output);
    assert_eq!(result["model_tools"]["mode"], "disabled");
    assert_eq!(result["model_tools"]["withheld_tool_results"], 1);
    assert!(!requests[0].to_string().contains(note));
    assert!(
        user_texts(&requests[0])
            .iter()
            .any(|text| text.starts_with("Tool result withheld"))
    );
    let events = fs::read_to_string(&journal).expect("journal");
    assert!(
        events.contains(note),
        "the journal keeps the original result"
    );
}

#[test]
fn a_length_limited_tool_mode_reply_reports_its_finish_reason() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    let server = SequenceServer::start(vec![json!({
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "A partial"},
            "finish_reason": "length"
        }]
    })]);
    let output = fixture.chat(&journal, &server.endpoint, "say a lot");
    server.finish();

    assert_success(&output);
    let result = json_output(&output);
    assert_eq!(result["model_tools"]["mode"], "enabled");
    assert_eq!(result["reply"]["content"], "A partial");
    assert_eq!(result["reply"]["finish_reason"], "length");
    assert_eq!(result["model_tools"]["withheld_tool_results"], 0);
}

#[test]
fn model_tool_calls_never_read_outside_the_workspace_through_the_cli() {
    let attempts = [
        json!({"path": "../outside.txt"}),
        json!({"path": "../../beyond.txt"}),
        json!({"path": "outside-hardlink"}),
    ];
    for arguments in attempts {
        let fixture = Fixture::new(NOTE);
        fs::write(fixture.root().join("outside.txt"), IN_ROOT).expect("in-root sentinel");
        let beyond = fixture
            .root()
            .parent()
            .expect("temporary")
            .join("beyond.txt");
        fs::write(&beyond, BEYOND_ROOT).expect("beyond-root sentinel");
        fs::hard_link(&beyond, fixture.workspace.join("outside-hardlink")).expect("hard link");
        let journal = fixture.session();
        fixture.set("--model-tools", true);

        // Serve only the escape attempt. A refused call ends the turn; any
        // follow-up request would fail to connect, which also discloses nothing.
        let server =
            SequenceServer::start(vec![tool_call("escape", "workspace_read_file", &arguments)]);
        let output = fixture.chat(&journal, &server.endpoint, "read outside");
        let requests = server.finish();

        let transcript = format!(
            "{}{}{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            fs::read_to_string(&journal).expect("journal"),
            serde_json::to_string(&requests).expect("requests"),
        );
        assert!(
            !transcript.contains(IN_ROOT) && !transcript.contains(BEYOND_ROOT),
            "{arguments}: an outside sentinel escaped"
        );
    }
}

#[test]
fn revoking_read_denies_model_reads_even_with_tools_enabled() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    fixture.set("--read", false);

    let server = SequenceServer::start(vec![tool_call(
        "call-1",
        "workspace_read_file",
        &json!({"path": "note.txt"}),
    )]);
    let output = fixture.chat(&journal, &server.endpoint, "read note.txt");
    let requests = server.finish();

    assert_error_code(&output, "permission");
    assert_eq!(
        requests.len(),
        1,
        "a denial stops before a follow-up request"
    );
    let events = fs::read_to_string(&journal).expect("journal");
    assert!(events.contains("permission_denied"));
    assert!(!events.contains(std::str::from_utf8(NOTE).expect("UTF-8 note")));
}

/// Start chat under the delegated cgroup a confined run needs.
///
/// A chat turn can perform an approved model command in the same process, and
/// on Linux the trusted runner refuses a process outside a delegated cgroup.
/// `run_command` places the CLI in a `Delegate=yes` scope there and is a plain
/// CLI launcher on Windows.
fn scoped_chat(scene: &Scene, journal: &Path, endpoint: &str, message: &str) -> Output {
    let journal = path_arg(journal);
    let arguments = [
        "chat",
        "--journal",
        journal,
        "--endpoint",
        endpoint,
        "--model",
        "fixture",
        "--context-tokens",
        "131072",
        "--max-output-tokens",
        "64",
        message,
    ];
    let result = run_with_deadline(
        run_command(&scene.data, &arguments),
        Duration::from_secs(60),
    )
    .expect("run OMGCode chat");
    assert!(
        !result.timed_out,
        "chat exceeded its deadline: stdout={} stderr={}",
        String::from_utf8_lossy(&result.output.stdout),
        String::from_utf8_lossy(&result.output.stderr)
    );
    result.output
}

/// The full operator flow: the model asks for one command, the operator
/// approves the exact request between turns, and the approved turn runs a real
/// confined program through the trusted runner on the host.
#[test]
fn an_approved_model_command_runs_successfully_through_the_trusted_runner() {
    let scene = Scene::build(true, true);
    for permission in ["--read", "--write", "--model-tools"] {
        scene.set_permission(permission, "true");
    }
    assert_success(&scene.add_profile("system", &[]));

    let session = scene.cli(&["session", "new", "--workspace", path_arg(&scene.workspace)]);
    assert_success(&session);
    let journal = PathBuf::from(json_output(&session)["journal"].as_str().expect("journal"));

    // The platform shell is the program; the tool itself has no shell mode and
    // passes every argument exactly as the model provided it.
    let command = shell("echo approved");
    let request = json!({
        "profile": "system",
        "program": command[0],
        "arguments": &command[1..],
    });

    let server = SequenceServer::start(vec![
        tool_call("call-1", "workspace_run_command", &request),
        text("Waiting for operator approval."),
    ]);
    let first = scoped_chat(&scene, &journal, &server.endpoint, "run the command");
    let requests = server.finish();

    assert_success(&first);
    let first = json_output(&first);
    assert_eq!(first["reply"]["content"], "Waiting for operator approval.");
    assert_eq!(first["model_tools"]["tool_calls"], 1);
    assert_eq!(requests.len(), 2);
    let offered = requests[0]["tools"].to_string();
    assert!(
        offered.contains("workspace_run_command"),
        "the command tool must be offered: {offered}"
    );

    let listed = scene.cli(&["approvals", "list"]);
    assert_success(&listed);
    let approvals = json_output(&listed)["approvals"]
        .as_array()
        .expect("approvals")
        .to_vec();
    assert_eq!(approvals.len(), 1, "one pending request: {approvals:?}");
    assert_eq!(approvals[0]["operation"], "command");
    assert_eq!(approvals[0]["state"], "pending");
    let approval = approvals[0]["id"].as_str().expect("approval id").to_owned();
    assert_success(&scene.cli(&["approvals", "approve", &approval]));

    let server = SequenceServer::start(vec![
        tool_call("call-2", "workspace_run_command", &request),
        text("The command finished."),
    ]);
    let second = scoped_chat(&scene, &journal, &server.endpoint, "run the command");
    let requests = server.finish();

    assert_success(&second);
    let second = json_output(&second);
    assert_eq!(second["reply"]["content"], "The command finished.");
    assert_eq!(second["model_tools"]["tool_calls"], 1);
    assert_eq!(requests.len(), 2);
    let result = user_texts(&requests[1])
        .into_iter()
        .find(|text| text.starts_with("Tool result [call call-2"))
        .expect("executed command result sent to the model");
    assert!(
        result.contains("\"status\":\"executed\""),
        "the model must receive the executed result: {result}"
    );
    let expected_stdout = if cfg!(windows) {
        "\"stdout\":\"approved\\r\\n\""
    } else {
        "\"stdout\":\"approved\\n\""
    };
    assert!(
        result.contains(expected_stdout),
        "the confined program output must reach the model: {result}"
    );

    let events = fs::read_to_string(&journal).expect("session journal");
    assert!(events.contains("tool.completed"));
    assert!(
        events.contains("\\\"status\\\":\\\"executed\\\""),
        "the session journal must record the executed result: {events}"
    );

    let audit = scene.audit();
    assert_eq!(audit["pending"], json!([]));
    let finished: Vec<&Value> = audit["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .filter(|entry| entry["action"]["type"] == "run_finished")
        .collect();
    assert_eq!(finished.len(), 1, "exactly one run: {audit}");
    assert_eq!(
        finished[0]["action"]["payload"]["finish"]["result"],
        json!({"kind": "exited", "code": 0}),
        "the confined program must exit successfully"
    );

    let listed = scene.cli(&["approvals", "list"]);
    assert_success(&listed);
    let listed = json_output(&listed);
    let approvals = listed["approvals"].as_array().expect("approvals");
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0]["state"], "consumed");

    scene.close();
}

#[test]
fn invalid_settings_stop_chat_before_any_model_request() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    let settings = fixture.data.join("control-v1.jsonl");
    let mut bytes = fs::read(&settings).expect("settings");
    bytes.extend_from_slice(b"{\"incomplete\":");
    fs::write(&settings, bytes).expect("corrupt settings tail");

    // Nothing listens here; a request would fail with a provider error instead.
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve port");
    let endpoint = format!("http://{}/v1", listener.local_addr().expect("address"));
    drop(listener);
    let output = fixture.chat(&journal, &endpoint, "read note.txt");

    support::assert_control_error(&output);
    let events = fs::read_to_string(&journal).expect("journal");
    assert!(!events.contains("turn.started"));
}
