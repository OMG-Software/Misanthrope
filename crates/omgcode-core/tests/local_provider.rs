use omgcode_core::prompt::{BASE_PROMPT, ModelMessage, ModelRole};
use omgcode_core::provider::{
    LocalModelClient, LocalModelConfig, ModelReply, ProviderError, Usage,
};
use omgcode_core::turn::{ToolCall, ToolModelReply, tool_definitions};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const SUCCESS: &str = r#"{"choices":[{"message":{"role":"assistant","content":"A plain reply."},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":4,"total_tokens":16}}"#;
const MAX_REQUEST_HEADER_BYTES: usize = 16 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 256 * 1024;

struct Fixture {
    address: String,
    task: JoinHandle<(String, Vec<u8>)>,
}

impl Fixture {
    /// Serve one bounded local exchange without depending on production retries.
    fn start(status: &str, body: &[u8], extra_headers: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local fixture");
        listener
            .set_nonblocking(true)
            .expect("make fixture accept bounded");
        let address = listener.local_addr().expect("fixture address");
        let body = body.to_vec();
        let extra_headers = extra_headers.to_owned();
        let status = status.to_owned();
        let task = thread::spawn(move || {
            let accept_deadline = Instant::now() + Duration::from_secs(5);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < accept_deadline, "fixture accept timed out");
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                }
            };
            stream
                .set_nonblocking(false)
                .expect("make accepted fixture stream blocking");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("set fixture read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .expect("set fixture write timeout");
            let (head, request_body) = read_request(&mut stream).expect("read bounded request");
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write fixture response headers");
            stream.write_all(&body).expect("write fixture body");
            (head, request_body)
        });
        Self {
            address: format!("http://{address}/v1"),
            task,
        }
    }

    /// Observe fixture completion before returning captured headers and body.
    fn finish(self) -> (String, Vec<u8>) {
        join_bounded(self.task, "fixture completes")
    }
}

/// Fail independently when a mock worker does not terminate within its budget.
fn join_bounded<T>(task: JoinHandle<T>, message: &str) -> T {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !task.is_finished() {
        assert!(Instant::now() < deadline, "{message} within the deadline");
        thread::sleep(Duration::from_millis(2));
    }
    task.join().expect(message)
}

/// Validate fixture framing and cap allocations before reading a declared body.
fn read_request(stream: &mut TcpStream) -> std::io::Result<(String, Vec<u8>)> {
    let mut head = Vec::new();
    let mut window = Vec::with_capacity(4);
    while window.as_slice() != b"\r\n\r\n" {
        if head.len() >= MAX_REQUEST_HEADER_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "fixture request headers exceed their byte limit",
            ));
        }
        let mut byte = [0_u8; 1];
        stream.read_exact(&mut byte)?;
        head.push(byte[0]);
        if window.len() == 4 {
            window.remove(0);
        }
        window.push(byte[0]);
    }
    let head_text = String::from_utf8(head)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let content_length = head_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>())
        })
        .transpose()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "missing content length")
        })?;
    if content_length > MAX_REQUEST_BODY_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "fixture request body exceeds its byte limit",
        ));
    }
    let mut body = vec![0; content_length];
    stream.read_exact(&mut body)?;
    Ok((head_text, body))
}

fn messages() -> Vec<ModelMessage> {
    vec![
        ModelMessage {
            role: ModelRole::System,
            content: BASE_PROMPT.to_owned(),
        },
        ModelMessage {
            role: ModelRole::User,
            content: "Saved user preferences".to_owned(),
        },
        ModelMessage {
            role: ModelRole::Assistant,
            content: "Prior assistant text".to_owned(),
        },
        ModelMessage {
            role: ModelRole::User,
            content: "Last user request".to_owned(),
        },
    ]
}

fn client(address: String, token: Option<String>) -> LocalModelClient {
    LocalModelClient::new(
        LocalModelConfig {
            base_url: address,
            model: "already-loaded-model".to_owned(),
            context_tokens: 32_768,
            max_output_tokens: 512,
        },
        token,
    )
    .expect("valid local model client")
}

/// Observe the listening socket so rejection proves the prompt was not sent.
fn assert_prompt_rejected_before_connect(messages: &[ModelMessage]) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind no-call fixture");
    listener
        .set_nonblocking(true)
        .expect("make no-call fixture nonblocking");
    let address = format!(
        "http://{}/v1",
        listener.local_addr().expect("fixture address")
    );
    let model = client(address, None);
    let monitor = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_millis(250);
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .expect("make accepted fixture stream blocking");
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .expect("set fixture read timeout");
                    stream
                        .set_write_timeout(Some(Duration::from_secs(1)))
                        .expect("set fixture write timeout");
                    let (head, _) = read_request(&mut stream).expect("read bounded request");
                    stream
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                SUCCESS.len(),
                                SUCCESS
                            )
                            .as_bytes(),
                        )
                        .expect("reply to any unexpected request");
                    break Some(head);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        break None;
                    }
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("fixture accept failed: {error}"),
            }
        }
    });

    let result = model.complete(messages);
    let unexpected_request = join_bounded(monitor, "bounded listener monitor completes");

    assert_eq!(result, Err(ProviderError::InvalidConfiguration));
    assert!(
        unexpected_request.is_none(),
        "invalid prompt must not reach server"
    );
}

#[test]
fn sends_prompt_roles_without_tools_and_parses_plain_reply() {
    let fixture = Fixture::start("200 OK", SUCCESS.as_bytes(), "");
    let model = client(
        fixture.address.clone(),
        Some("local-secret-token".to_owned()),
    );

    let reply = model.complete(&messages()).expect("valid completion");
    let (head, body) = fixture.finish();
    let request: serde_json::Value = serde_json::from_slice(&body).expect("request JSON");

    assert!(head.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
    assert!(
        head.to_ascii_lowercase()
            .contains("authorization: bearer local-secret-token")
    );
    assert_eq!(request["model"], "already-loaded-model");
    assert_eq!(request["stream"], false);
    assert_eq!(request["max_tokens"], 512);
    assert_eq!(request["messages"][0]["role"], "system");
    assert_eq!(request["messages"][0]["content"], BASE_PROMPT);
    assert_eq!(request["messages"][1]["role"], "user");
    assert_eq!(request["messages"][2]["role"], "assistant");
    assert_eq!(request["messages"][3]["content"], "Last user request");
    assert!(request.get("tools").is_none());
    assert!(request.get("tool_choice").is_none());
    assert_eq!(
        reply,
        ModelReply {
            content: "A plain reply.".to_owned(),
            finish_reason: "stop".to_owned(),
            usage: Some(Usage {
                prompt_tokens: Some(12),
                completion_tokens: Some(4),
                total_tokens: Some(16),
            }),
        }
    );
}

#[test]
fn does_not_leak_token_on_authentication_failure() {
    let fixture = Fixture::start("401 Unauthorized", b"server echoed local-secret-token", "");
    let model = client(
        fixture.address.clone(),
        Some("local-secret-token".to_owned()),
    );
    let error = model.complete(&messages()).expect_err("401 is an error");
    let (head, _) = fixture.finish();

    assert_eq!(error, ProviderError::HttpStatus(401));
    assert!(!error.to_string().contains("local-secret-token"));
    assert!(!format!("{error:?}").contains("local-secret-token"));
    assert!(
        head.to_ascii_lowercase()
            .contains("authorization: bearer local-secret-token")
    );
}

#[test]
fn rejects_invalid_response_schemas_and_tool_requests() {
    let malformed = Fixture::start("200 OK", br#"{"choices":[]}"#, "");
    let error = client(malformed.address.clone(), None)
        .complete(&messages())
        .expect_err("empty choices are invalid");
    malformed.finish();
    assert_eq!(error, ProviderError::MalformedResponse);

    let tool_call = Fixture::start(
        "200 OK",
        br#"{"choices":[{"message":{"role":"assistant","content":"maybe","tool_calls":[{"type":"function"}]},"finish_reason":"tool_calls"}]}"#,
        "",
    );
    let error = client(tool_call.address.clone(), None)
        .complete(&messages())
        .expect_err("tool requests are unsupported");
    tool_call.finish();
    assert_eq!(error, ProviderError::UnsupportedToolCall);
}

#[test]
fn accepts_empty_lm_studio_tool_call_array_as_no_action() {
    let response = br#"{"choices":[{"message":{"role":"assistant","content":"READY.","tool_calls":[],"reasoning_content":""},"finish_reason":"stop"}],"usage":{"prompt_tokens":893,"completion_tokens":3,"total_tokens":896}}"#;
    let fixture = Fixture::start("200 OK", response, "");
    let model = client(fixture.address.clone(), None);

    let result = model.complete(&messages());
    fixture.finish();

    assert_eq!(
        result,
        Ok(ModelReply {
            content: "READY.".to_owned(),
            finish_reason: "stop".to_owned(),
            usage: Some(Usage {
                prompt_tokens: Some(893),
                completion_tokens: Some(3),
                total_tokens: Some(896),
            }),
        })
    );
}

#[test]
fn rejects_oversized_response_body() {
    let fixture = Fixture::start("200 OK", &vec![b'x'; 1024 * 1024 + 1], "");
    let error = client(fixture.address.clone(), None)
        .complete(&messages())
        .expect_err("oversized response is rejected");
    fixture.finish();
    assert_eq!(error, ProviderError::ResponseTooLarge);
}

#[test]
fn null_action_fields_are_absent_but_actual_or_malformed_calls_fail() {
    for (fields, expected) in [
        (
            serde_json::json!({"tool_calls":null,"function_call":null}),
            None,
        ),
        (
            serde_json::json!({"tool_calls":[{}]}),
            Some(ProviderError::UnsupportedToolCall),
        ),
        (
            serde_json::json!({"function_call":{}}),
            Some(ProviderError::UnsupportedToolCall),
        ),
        (
            serde_json::json!({"tool_calls":"not an array"}),
            Some(ProviderError::MalformedResponse),
        ),
    ] {
        // Exercise both response locations, including unexpected choice fields.
        for at_choice in [false, true] {
            let mut response = serde_json::json!({"choices":[{
                "message":{"role":"assistant","content":"plain text"},
                "finish_reason":"stop"
            }]});
            let target = if at_choice {
                response["choices"][0]
                    .as_object_mut()
                    .expect("choice object")
            } else {
                response["choices"][0]["message"]
                    .as_object_mut()
                    .expect("message object")
            };
            target.extend(fields.as_object().expect("field object").clone());
            let encoded = serde_json::to_vec(&response).expect("fixture response JSON");
            let fixture = Fixture::start("200 OK", &encoded, "");
            let result = client(fixture.address.clone(), None).complete(&messages());
            fixture.finish();
            assert_eq!(result.err(), expected);
        }
    }
}

#[test]
fn does_not_follow_redirects() {
    let fixture = Fixture::start(
        "302 Found",
        b"",
        "Location: http://127.0.0.1:9/v1/chat/completions\r\n",
    );
    let error = client(fixture.address.clone(), None)
        .complete(&messages())
        .expect_err("redirect response is not followed");
    fixture.finish();
    assert_eq!(error, ProviderError::HttpStatus(302));
}

#[test]
fn rejects_destinations_outside_literal_loopback_http_before_connecting() {
    for base_url in [
        "http://localhost:1234/v1",
        "http://192.0.2.1:1234/v1",
        "https://127.0.0.1:1234/v1",
        "http://127.0.0.1/v1",
        "http://127.0.0.1:0/v1",
        "http://127.0.0.1:1234/v1/other",
        "http://user@127.0.0.1:1234/v1",
        "http://127.0.0.1:1234/v1?query=1",
        "http://127.0.0.1:1234/v1#fragment",
    ] {
        let result = LocalModelClient::new(
            LocalModelConfig {
                base_url: base_url.to_owned(),
                model: "model".to_owned(),
                context_tokens: 4096,
                max_output_tokens: 128,
            },
            None,
        );
        assert_eq!(
            result.err(),
            Some(ProviderError::InvalidConfiguration),
            "{base_url}"
        );
    }
}

#[test]
fn context_budget_fails_before_network_request() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind no-call fixture");
    listener
        .set_nonblocking(true)
        .expect("make no-call fixture nonblocking");
    let address = format!(
        "http://{}/v1",
        listener.local_addr().expect("fixture address")
    );
    let model = LocalModelClient::new(
        LocalModelConfig {
            base_url: address,
            model: "model".to_owned(),
            context_tokens: 1,
            max_output_tokens: 1,
        },
        None,
    )
    .expect("valid limits");

    assert_eq!(
        model.complete(&messages()),
        Err(ProviderError::ContextLimit)
    );
    assert!(
        listener.accept().is_err(),
        "budget rejection must not connect"
    );
}

#[test]
fn requires_exact_compiled_base_as_the_only_system_message() {
    let mut missing_base = messages();
    missing_base.remove(0);
    assert_prompt_rejected_before_connect(&missing_base);

    let mut changed_base = messages();
    changed_base[0].content.push(' ');
    assert_prompt_rejected_before_connect(&changed_base);

    let mut extra_system = messages();
    extra_system.push(ModelMessage {
        role: ModelRole::System,
        content: "Untrusted extra system message".to_owned(),
    });
    assert_prompt_rejected_before_connect(&extra_system);
}

#[test]
fn wire_size_limit_fails_before_network_request() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind no-call fixture");
    listener
        .set_nonblocking(true)
        .expect("make no-call fixture nonblocking");
    let address = format!(
        "http://{}/v1",
        listener.local_addr().expect("fixture address")
    );
    let model = LocalModelClient::new(
        LocalModelConfig {
            base_url: address,
            model: "model".to_owned(),
            context_tokens: 2_000_000,
            max_output_tokens: 1,
        },
        None,
    )
    .expect("valid limits");
    let oversized = [
        ModelMessage {
            role: ModelRole::System,
            content: BASE_PROMPT.to_owned(),
        },
        ModelMessage {
            role: ModelRole::User,
            content: "x".repeat(300 * 1024),
        },
    ];

    assert_eq!(
        model.complete(&oversized),
        Err(ProviderError::RequestTooLarge)
    );
    assert!(
        listener.accept().is_err(),
        "size rejection must not connect"
    );
}

#[test]
fn rejects_control_characters_in_bearer_token() {
    let result = LocalModelClient::new(
        LocalModelConfig {
            base_url: "http://127.0.0.1:1234/v1".to_owned(),
            model: "model".to_owned(),
            context_tokens: 4096,
            max_output_tokens: 128,
        },
        Some("bad\r\ntoken".to_owned()),
    );

    assert_eq!(result.err(), Some(ProviderError::InvalidToken));
}

const TOOL_CALL_REPLY: &str = r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call-1","type":"function","function":{"name":"workspace_read_file","arguments":"{\"path\":\"notes.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#;

/// Send one tool-enabled request to a fixture serving `body`.
fn tool_exchange(body: &str) -> (Result<ToolModelReply, ProviderError>, Value) {
    let fixture = Fixture::start("200 OK", body.as_bytes(), "");
    let result =
        client(fixture.address.clone(), None).complete_with_tools(&messages(), &tool_definitions());
    let (_, request) = fixture.finish();
    (
        result,
        serde_json::from_slice(&request).expect("request JSON"),
    )
}

#[test]
fn tool_requests_advertise_exact_function_tools_and_return_raw_calls() {
    let (reply, request) = tool_exchange(TOOL_CALL_REPLY);
    let reply = reply.expect("tool-call reply");

    assert_eq!(request["tool_choice"], "auto");
    assert_eq!(request["stream"], false);
    let tools = request["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| {
            assert_eq!(tool["type"], "function");
            tool["function"]["name"].as_str().expect("tool name")
        })
        .collect();
    assert_eq!(names, ["workspace_read_file", "workspace_list_directory"]);
    assert_eq!(
        tools[0]["function"]["parameters"]["additionalProperties"],
        false
    );
    let roles: Vec<&str> = request["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .map(|message| message["role"].as_str().expect("role"))
        .collect();
    assert_eq!(roles, ["system", "user", "assistant", "user"]);

    assert_eq!(reply.content, "");
    assert_eq!(reply.tool_calls.len(), 1);
    // The adapter returns calls unvalidated; the runtime parses them strictly.
    assert_eq!(reply.tool_calls[0]["id"], "call-1");
    assert!(ToolCall::parse(&reply.tool_calls[0]).is_ok());
}

#[test]
fn tool_requests_accept_plain_text_and_empty_call_lists() {
    for body in [
        SUCCESS,
        r#"{"choices":[{"message":{"role":"assistant","content":"done","tool_calls":[]},"finish_reason":"stop"}]}"#,
    ] {
        let (reply, _) = tool_exchange(body);
        let reply = reply.expect("text reply");
        assert!(reply.tool_calls.is_empty());
        assert!(!reply.content.is_empty());
    }
}

#[test]
fn tool_responses_with_duplicate_keys_or_unsupported_shapes_are_refused() {
    let many_calls = format!(
        r#"{{"choices":[{{"message":{{"role":"assistant","content":null,"tool_calls":[{}]}},"finish_reason":"tool_calls"}}]}}"#,
        vec![r#"{"id":"x","type":"function","function":{"name":"workspace_list_directory","arguments":"{}"}}"#; 65].join(",")
    );
    let cases = [
        // Duplicate keys could smuggle a second tool name or argument string.
        r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"a","type":"function","function":{"name":"workspace_read_file","name":"workspace_delete","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
        r#"{"choices":[{"message":{"role":"assistant","role":"system","content":"x"},"finish_reason":"stop"}]}"#,
        // A non-assistant role is refused.
        r#"{"choices":[{"message":{"role":"system","content":"x"},"finish_reason":"stop"}]}"#,
        // Null content needs at least one tool call.
        r#"{"choices":[{"message":{"role":"assistant","content":null},"finish_reason":"stop"}]}"#,
        // Truncated tool-call output is never dispatched.
        r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"a","type":"function","function":{"name":"workspace_read_file","arguments":"{\"path\":"}}]},"finish_reason":"length"}]}"#,
        r#"{"choices":[{"message":{"role":"assistant","content":"x","tool_calls":{}},"finish_reason":"stop"}]}"#,
        r#"{"choices":[]}"#,
        &many_calls,
    ];
    for body in cases {
        let (reply, _) = tool_exchange(body);
        assert_eq!(
            reply.expect_err("refused tool response"),
            ProviderError::MalformedResponse,
            "{body}"
        );
    }

    for body in [
        r#"{"choices":[{"message":{"role":"assistant","content":null,"function_call":{"name":"workspace_read_file","arguments":"{}"}},"finish_reason":"function_call"}]}"#,
        r#"{"choices":[{"message":{"role":"assistant","content":"x"},"tool_calls":[{"id":"a"}],"finish_reason":"stop"}]}"#,
    ] {
        let (reply, _) = tool_exchange(body);
        assert_eq!(
            reply.expect_err("legacy or misplaced call"),
            ProviderError::UnsupportedToolCall,
            "{body}"
        );
    }
}
