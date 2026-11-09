use crate::prompt::{BASE_PROMPT, ModelMessage, ModelRole};
use reqwest::Url;
use reqwest::blocking::{Client, Response};
use reqwest::header::{AUTHORIZATION, HeaderValue};
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::io::Read;
use std::net::IpAddr;
use std::time::Duration;
use thiserror::Error;

const MAX_REQUEST_BYTES: usize = 256 * 1024;
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MESSAGE_OVERHEAD_ESTIMATE: u64 = 256;
const PROMPT_HEADROOM_ESTIMATE: u64 = 512;

/// Explicit settings for one local LM Studio Chat Completions connection.
///
/// `context_tokens` is a required conservative budget supplied by the caller.
/// This adapter estimates input tokens from UTF-8 bytes and fixed overhead; it
/// does not claim to reproduce the selected model's tokenizer.
pub struct LocalModelConfig {
    /// A literal loopback HTTP URL with an explicit port and `/v1` path.
    pub base_url: String,
    /// The model identifier already available to the selected LM Studio server.
    pub model: String,
    /// Caller-validated context budget for this model, in tokens.
    pub context_tokens: u64,
    /// Maximum generated tokens for each request.
    pub max_output_tokens: u32,
}

/// A validated local LM Studio Chat Completions client.
pub struct LocalModelClient {
    client: Client,
    endpoint: Url,
    model: String,
    context_tokens: u64,
    max_output_tokens: u32,
    authorization: Option<HeaderValue>,
    request_timeout: Duration,
}

impl fmt::Debug for LocalModelClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalModelClient")
            .field("model", &self.model)
            .field("context_tokens", &self.context_tokens)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("has_token", &self.authorization.is_some())
            .finish()
    }
}

/// A successful assistant reply and provider-reported usage, when available.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelReply {
    /// Plain assistant text. This value is untrusted model output.
    pub content: String,
    /// The provider finish reason (`stop` or `length`).
    pub finish_reason: String,
    /// Optional usage counters returned by the server.
    pub usage: Option<Usage>,
}

/// Optional token counters reported by the provider.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Usage {
    /// Input tokens, when reported.
    pub prompt_tokens: Option<u64>,
    /// Generated tokens, when reported.
    pub completion_tokens: Option<u64>,
    /// Combined tokens, when reported.
    pub total_tokens: Option<u64>,
}

/// Safe failure categories for local model configuration and inference.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ProviderError {
    /// The server URL, model, or explicit budgets are invalid.
    #[error("invalid local model configuration")]
    InvalidConfiguration,
    /// The optional bearer token cannot be represented safely in an HTTP header.
    #[error("invalid local model token")]
    InvalidToken,
    /// The serialized request exceeds the wire byte limit.
    #[error("local model request exceeds 256 KiB")]
    RequestTooLarge,
    /// The conservative request estimate exceeds the configured context budget.
    #[error("local model request exceeds the configured context budget")]
    ContextLimit,
    /// The request failed before a valid HTTP response arrived.
    #[error("local model transport failed")]
    Transport,
    /// The local server returned a non-success status.
    #[error("local model server returned HTTP status {0}")]
    HttpStatus(u16),
    /// The server response exceeds the byte limit.
    #[error("local model response exceeds 1 MiB")]
    ResponseTooLarge,
    /// The server response does not match the supported response schema.
    #[error("invalid local model response")]
    MalformedResponse,
    /// The server requested a tool or function call, which this adapter does not support.
    #[error("local model tool calls are not supported")]
    UnsupportedToolCall,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ModelMessage],
    stream: bool,
    max_tokens: u32,
}

impl LocalModelClient {
    /// Create a client for a literal loopback LM Studio endpoint.
    ///
    /// The constructor does not start LM Studio, load a model, or download one.
    /// It rejects hostnames, non-loopback addresses, redirects, proxies, and
    /// URLs that could route a credential or prompt to another destination.
    pub fn new(config: LocalModelConfig, token: Option<String>) -> Result<Self, ProviderError> {
        Self::with_timeout(config, token, REQUEST_TIMEOUT)
    }

    /// Build the same validated transport with a short deadline in unit tests.
    /// Production callers always use the fixed thirty-second deadline in `new`.
    fn with_timeout(
        config: LocalModelConfig,
        token: Option<String>,
        request_timeout: Duration,
    ) -> Result<Self, ProviderError> {
        let mut endpoint = validate_base_url(&config.base_url)?;
        if config.model.is_empty()
            || config.model.len() > 256
            || config.model.chars().any(char::is_control)
            || config.context_tokens == 0
            || config.max_output_tokens == 0
        {
            return Err(ProviderError::InvalidConfiguration);
        }
        endpoint.set_path("/v1/chat/completions");

        let authorization = token
            .map(|token| {
                if token.is_empty() || token.chars().any(char::is_control) {
                    return Err(ProviderError::InvalidToken);
                }
                let mut header = HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|_| ProviderError::InvalidToken)?;
                header.set_sensitive(true);
                Ok(header)
            })
            .transpose()?;

        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(request_timeout)
            .build()
            .map_err(|_| ProviderError::Transport)?;

        Ok(Self {
            client,
            endpoint,
            model: config.model,
            context_tokens: config.context_tokens,
            max_output_tokens: config.max_output_tokens,
            authorization,
            request_timeout,
        })
    }

    /// Send one bounded, non-streaming Chat Completions request.
    ///
    /// The adapter sends only model, role-tagged messages, `stream=false`, and
    /// `max_tokens`. It never sends tool definitions or hosted execution fields.
    /// Model output remains untrusted plain text and is never interpreted as an
    /// action by this adapter.
    pub fn complete(&self, messages: &[ModelMessage]) -> Result<ModelReply, ProviderError> {
        let Some((base_message, following_messages)) = messages.split_first() else {
            return Err(ProviderError::InvalidConfiguration);
        };
        if base_message.role != ModelRole::System
            || base_message.content != BASE_PROMPT
            || following_messages
                .iter()
                .any(|message| message.role == ModelRole::System)
        {
            return Err(ProviderError::InvalidConfiguration);
        }

        let estimate = estimate_context(messages, self.max_output_tokens)?;
        if estimate > self.context_tokens {
            return Err(ProviderError::ContextLimit);
        }
        if estimate_serialized_request_size(messages, &self.model)? > MAX_REQUEST_BYTES as u64 {
            return Err(ProviderError::RequestTooLarge);
        }

        let body = serde_json::to_vec(&ChatRequest {
            model: &self.model,
            messages,
            stream: false,
            max_tokens: self.max_output_tokens,
        })
        .map_err(|_| ProviderError::MalformedResponse)?;
        if body.len() > MAX_REQUEST_BYTES {
            return Err(ProviderError::RequestTooLarge);
        }

        let mut request = self
            .client
            .post(self.endpoint.clone())
            .timeout(self.request_timeout)
            .header(reqwest::header::CONTENT_TYPE, "application/json");
        if let Some(authorization) = &self.authorization {
            request = request.header(AUTHORIZATION, authorization.clone());
        }
        let response = request
            .body(body)
            .send()
            .map_err(|_| ProviderError::Transport)?;
        parse_response(response)
    }
}

/// Reject destinations which could redirect prompts or credentials off loopback.
fn validate_base_url(base_url: &str) -> Result<Url, ProviderError> {
    if base_url.chars().any(char::is_control) || !base_url.starts_with("http://") {
        return Err(ProviderError::InvalidConfiguration);
    }
    let after_scheme = &base_url["http://".len()..];
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    if authority.is_empty() || authority.contains('@') || authority.contains('%') {
        return Err(ProviderError::InvalidConfiguration);
    }

    let (host, raw_port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let close = bracketed
            .find(']')
            .ok_or(ProviderError::InvalidConfiguration)?;
        let host = &bracketed[..close];
        let suffix = &bracketed[close + 1..];
        let port = suffix
            .strip_prefix(':')
            .ok_or(ProviderError::InvalidConfiguration)?;
        (host, port)
    } else {
        let (host, port) = authority
            .rsplit_once(':')
            .ok_or(ProviderError::InvalidConfiguration)?;
        if host.contains(':') {
            return Err(ProviderError::InvalidConfiguration);
        }
        (host, port)
    };
    let address: IpAddr = host
        .parse()
        .map_err(|_| ProviderError::InvalidConfiguration)?;
    let _port = raw_port
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or(ProviderError::InvalidConfiguration)?;
    if !address.is_loopback() {
        return Err(ProviderError::InvalidConfiguration);
    }

    let parsed = Url::parse(base_url).map_err(|_| ProviderError::InvalidConfiguration)?;
    if parsed.scheme() != "http"
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !matches!(parsed.path(), "/v1" | "/v1/")
    {
        return Err(ProviderError::InvalidConfiguration);
    }
    Ok(parsed)
}

/// Bound input with a byte-based estimate and reserve output plus template headroom.
fn estimate_context(
    messages: &[ModelMessage],
    max_output_tokens: u32,
) -> Result<u64, ProviderError> {
    let message_count = u64::try_from(messages.len()).map_err(|_| ProviderError::ContextLimit)?;
    let content_bytes = messages.iter().try_fold(0_u64, |total, message| {
        let size =
            u64::try_from(message.content.len()).map_err(|_| ProviderError::RequestTooLarge)?;
        total
            .checked_add(size)
            .ok_or(ProviderError::RequestTooLarge)
    })?;
    content_bytes
        .checked_add(
            message_count
                .checked_mul(MESSAGE_OVERHEAD_ESTIMATE)
                .ok_or(ProviderError::ContextLimit)?,
        )
        .and_then(|total| total.checked_add(PROMPT_HEADROOM_ESTIMATE))
        .and_then(|total| total.checked_add(u64::from(max_output_tokens)))
        .ok_or(ProviderError::ContextLimit)
}

/// Bound escaped strings before allocating the complete encoded request.
fn estimate_serialized_request_size(
    messages: &[ModelMessage],
    model: &str,
) -> Result<u64, ProviderError> {
    let encoded_strings = messages
        .iter()
        .try_fold(json_string_len(model)?, |total, message| {
            total
                .checked_add(json_string_len(&message.content)?)
                .ok_or(ProviderError::RequestTooLarge)
        })?;
    let message_overhead = u64::try_from(messages.len())
        .map_err(|_| ProviderError::RequestTooLarge)?
        .checked_mul(64)
        .ok_or(ProviderError::RequestTooLarge)?;
    encoded_strings
        .checked_add(message_overhead)
        .and_then(|total| total.checked_add(128))
        .ok_or(ProviderError::RequestTooLarge)
}

/// Count JSON escape expansion with checked arithmetic rather than allocating it.
fn json_string_len(value: &str) -> Result<u64, ProviderError> {
    value.chars().try_fold(2_u64, |length, character| {
        let encoded_len = match character {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{0008}' | '\u{000c}' => 2,
            '\u{0000}'..='\u{001f}' => 6,
            _ => u64::try_from(character.len_utf8()).map_err(|_| ProviderError::RequestTooLarge)?,
        };
        length
            .checked_add(encoded_len)
            .ok_or(ProviderError::RequestTooLarge)
    })
}

/// Parse one bounded assistant response; never expose server error bodies.
fn parse_response(response: Response) -> Result<ModelReply, ProviderError> {
    let status = response.status();
    if !status.is_success() {
        return Err(ProviderError::HttpStatus(status.as_u16()));
    }

    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ProviderError::Transport)?;
    if u64::try_from(bytes.len()).map_err(|_| ProviderError::ResponseTooLarge)? > MAX_RESPONSE_BYTES
    {
        return Err(ProviderError::ResponseTooLarge);
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| ProviderError::MalformedResponse)?;
    let choices = value
        .get("choices")
        .and_then(Value::as_array)
        .filter(|choices| choices.len() == 1)
        .ok_or(ProviderError::MalformedResponse)?;
    let choice = choices.first().ok_or(ProviderError::MalformedResponse)?;
    let message = choice
        .get("message")
        .and_then(Value::as_object)
        .ok_or(ProviderError::MalformedResponse)?;
    reject_actions(message.get("tool_calls"), message.get("function_call"))?;
    reject_actions(choice.get("tool_calls"), choice.get("function_call"))?;
    if matches!(
        choice.get("finish_reason").and_then(Value::as_str),
        Some("tool_calls" | "function_call")
    ) {
        return Err(ProviderError::UnsupportedToolCall);
    }

    let role = message
        .get("role")
        .and_then(Value::as_str)
        .ok_or(ProviderError::MalformedResponse)?;
    if role != "assistant" {
        return Err(ProviderError::MalformedResponse);
    }
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .ok_or(ProviderError::MalformedResponse)?;
    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .filter(|reason| matches!(*reason, "stop" | "length"))
        .ok_or(ProviderError::MalformedResponse)?;
    let usage = match value.get("usage") {
        None | Some(Value::Null) => None,
        Some(usage) => Some(parse_usage(usage)?),
    };

    Ok(ModelReply {
        content: content.to_owned(),
        finish_reason: finish_reason.to_owned(),
        usage,
    })
}

/// Accept explicit empty action fields while refusing all actual action requests.
fn reject_actions(
    tool_calls: Option<&Value>,
    function_call: Option<&Value>,
) -> Result<(), ProviderError> {
    match tool_calls {
        None | Some(Value::Null) => {}
        // LM Studio includes an empty array in ordinary text-only replies.
        Some(Value::Array(calls)) if calls.is_empty() => {}
        Some(Value::Array(_)) => return Err(ProviderError::UnsupportedToolCall),
        Some(_) => return Err(ProviderError::MalformedResponse),
    }
    if function_call.is_some_and(|call| !call.is_null()) {
        return Err(ProviderError::UnsupportedToolCall);
    }
    Ok(())
}

/// Preserve optional provider counters without interpreting them as verified cost.
fn parse_usage(value: &Value) -> Result<Usage, ProviderError> {
    let object = value.as_object().ok_or(ProviderError::MalformedResponse)?;
    /// Refuse non-integer or negative counters; absence is not a fabricated zero.
    fn optional_count(
        object: &serde_json::Map<String, Value>,
        key: &str,
    ) -> Result<Option<u64>, ProviderError> {
        match object.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_u64()
                .map(Some)
                .ok_or(ProviderError::MalformedResponse),
        }
    }

    Ok(Usage {
        prompt_tokens: optional_count(object, "prompt_tokens")?,
        completion_tokens: optional_count(object, "completion_tokens")?,
        total_tokens: optional_count(object, "total_tokens")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Instant;

    /// Give the server its own deadline and cancellation path, independent of HTTP timeouts.
    fn slow_response(
        drip_body: bool,
    ) -> (LocalModelClient, mpsc::Sender<()>, thread::JoinHandle<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("private fixture");
        listener.set_nonblocking(true).expect("bounded accept");
        let address = listener.local_addr().expect("address");
        let (cancel, cancelled) = mpsc::channel();
        let task = thread::spawn(move || {
            let end = Instant::now() + Duration::from_secs(2);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < end, "fixture accept deadline");
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            };
            stream
                .set_nonblocking(false)
                .expect("blocking accepted stream");
            stream
                .set_read_timeout(Some(Duration::from_millis(250)))
                .expect("read deadline");
            stream
                .set_write_timeout(Some(Duration::from_millis(250)))
                .expect("write deadline");
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 8192];
            loop {
                assert!(
                    bytes.len() <= 256 * 1024 && Instant::now() < end,
                    "bounded request"
                );
                if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    let headers = std::str::from_utf8(&bytes[..header_end]).expect("ASCII headers");
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().expect("body length"))
                        })
                        .expect("Content-Length");
                    assert!(length <= 256 * 1024, "bounded declared request");
                    if bytes.len() >= header_end + 4 + length {
                        break;
                    }
                }
                let count = stream.read(&mut buffer).expect("request bytes");
                assert!(count > 0, "request must be complete");
                bytes.extend_from_slice(&buffer[..count]);
            }
            if drip_body {
                let body = br#"{"choices":[{"message":{"role":"assistant","content":"completed slowly"},"finish_reason":"stop"}]}"#;
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                if stream.write_all(headers.as_bytes()).is_err() {
                    return true;
                }
                for chunk in body.chunks(8) {
                    if Instant::now() >= end || stream.write_all(chunk).is_err() {
                        break;
                    }
                    match cancelled.recv_timeout(Duration::from_millis(80)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            } else {
                // Withhold headers, but never wait indefinitely for a faulty client.
                match cancelled.recv_timeout(Duration::from_secs(2)) {
                    Ok(())
                    | Err(mpsc::RecvTimeoutError::Disconnected | mpsc::RecvTimeoutError::Timeout) =>
                        {}
                }
            }
            true
        });
        let client = LocalModelClient::with_timeout(
            LocalModelConfig {
                base_url: format!("http://{address}/v1"),
                model: "fixture".to_owned(),
                context_tokens: 131_072,
                max_output_tokens: 32,
            },
            None,
            Duration::from_millis(250),
        )
        .expect("client");
        (client, cancel, task)
    }

    /// Observe fixture cleanup even when the timeout assertion is expected to fail.
    fn assert_bounded_transport_failure(drip_body: bool) {
        let (client, cancel, task) = slow_response(drip_body);
        let started = Instant::now();
        let result = client.complete(&[
            ModelMessage {
                role: ModelRole::System,
                content: BASE_PROMPT.to_owned(),
            },
            ModelMessage {
                role: ModelRole::User,
                content: "request".to_owned(),
            },
        ]);
        let elapsed = started.elapsed();
        // Receiver disconnection also requests cleanup; either outcome is safe.
        // A disconnected receiver means the fixture is already exiting. The
        // bounded join below still observes completion in either case.
        let _ = cancel.send(());
        let end = Instant::now() + Duration::from_secs(3);
        while !task.is_finished() {
            assert!(Instant::now() < end, "fixture cleanup deadline");
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            task.join().expect("fixture thread"),
            "fixture received complete request"
        );
        assert_eq!(result, Err(ProviderError::Transport));
        assert!(
            elapsed < Duration::from_secs(1),
            "request exceeded its independent total deadline: {elapsed:?}"
        );
    }

    #[test]
    fn request_deadline_bounds_stalled_response_headers() {
        assert_bounded_transport_failure(false);
    }

    #[test]
    fn request_deadline_bounds_a_dripping_response_body() {
        assert_bounded_transport_failure(true);
    }
}
