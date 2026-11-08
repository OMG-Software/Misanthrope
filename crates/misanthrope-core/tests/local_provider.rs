use misanthrope_core::prompt::{BASE_PROMPT, ModelMessage, ModelRole};
use misanthrope_core::provider::{
    LocalModelClient, LocalModelConfig, ModelReply, ProviderError, Usage,
};
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

    fn finish(self) -> (String, Vec<u8>) {
        join_bounded(self.task, "fixture completes")
    }
}

fn join_bounded<T>(task: JoinHandle<T>, message: &str) -> T {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !task.is_finished() {
        assert!(Instant::now() < deadline, "{message} within the deadline");
        thread::sleep(Duration::from_millis(2));
    }
    task.join().expect(message)
}

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
