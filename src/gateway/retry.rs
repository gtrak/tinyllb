use crate::config::{RetryPolicy, TransientRetry};
use crate::gateway::error::ProxyError;
use serde_json::Value;

// ---------------------------------------------------------------------------
// is_premature_stop
// ---------------------------------------------------------------------------

/// Returns `true` iff the response is a recognizable chat completion with
/// `finish_reason == "stop"`, no content, and no tool calls — i.e. a
/// degenerate turn that kills the agentic thread.
// @lat: [[gateway#Premature-Stop Retry]]
pub fn is_premature_stop(body: &Value) -> bool {
    let choices = match body.get("choices") {
        Some(Value::Array(arr)) if !arr.is_empty() => arr,
        _ => return false,
    };

    let first = match &choices[0] {
        Value::Object(map) => map,
        _ => return false,
    };

    // finish_reason must be "stop" or "length" (both degenerate when empty).
    match first.get("finish_reason") {
        Some(Value::String(s)) if s == "stop" || s == "length" => {}
        _ => return false,
    };

    let message = match first.get("message") {
        Some(Value::Object(m)) => m,
        _ => return false,
    };

    // content: absent, null, or empty string => no content.
    // Non-string content (array, number, etc.) => treated as having content.
    let has_content = match message.get("content") {
        None => false,
        Some(Value::Null) => false,
        Some(Value::String(s)) if s.is_empty() => false,
        Some(Value::String(_)) => true,
        // Non-string types (array, number, bool, object) count as content.
        Some(_) => true,
    };
    if has_content {
        return false;
    }

    // tool_calls: absent, null, or empty array => no tool_calls.
    // Non-empty array => has tool_calls.
    let has_tool_calls = match message.get("tool_calls") {
        None => false,
        Some(Value::Null) => false,
        Some(Value::Array(arr)) => !arr.is_empty(),
        // Non-array, non-null types count as having tool_calls.
        Some(_) => true,
    };
    if has_tool_calls {
        return false;
    }

    // finish_reason == "stop" (checked above), no content, no tool_calls.
    true
}

// ---------------------------------------------------------------------------
// bump_temperature
// ---------------------------------------------------------------------------

/// Clone the request body and set `temperature` to
/// `min(base + attempt * step, max_temperature)`.
pub fn bump_temperature(body: &Value, attempt: u32, policy: &RetryPolicy) -> Value {
    let mut body_clone = body.clone();

    let base = match body.get("temperature") {
        Some(Value::Number(n)) if n.is_f64() => n.as_f64().unwrap_or(policy.default_temperature),
        _ => policy.default_temperature,
    };

    let new_temp = (base + attempt as f64 * policy.temperature_step).min(policy.max_temperature);

    let temp_val = serde_json::Number::from_f64(new_temp)
        .map(Value::Number)
        .unwrap_or_else(|| {
            serde_json::Number::from_f64(policy.default_temperature)
                .map(Value::Number)
                .unwrap_or(Value::Null)
        });

    body_clone["temperature"] = temp_val;
    body_clone
}

// ---------------------------------------------------------------------------
// FrameClassification + classify_frame
// ---------------------------------------------------------------------------

/// Classification of a single SSE event frame.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameClassification {
    /// `delta.content` is non-null and non-empty.
    pub has_content: bool,
    /// `delta.tool_calls` is non-null and non-empty.
    pub has_tool_calls: bool,
    /// `choices[0].finish_reason` string, if present in any data line.
    pub finish_reason: Option<String>,
    /// Literal `data: [DONE]` line detected.
    pub is_done: bool,
    /// Top-level `usage` object present and non-null.
    pub has_usage: bool,
}

/// Best-effort parse one SSE event frame (raw bytes).
///
/// Returns a [`FrameClassification`] with all fields defaulting to `false` /
/// `None` on parse failure.  Never panics.
pub fn classify_frame(frame: &[u8]) -> FrameClassification {
    let mut result = FrameClassification {
        has_content: false,
        has_tool_calls: false,
        finish_reason: None,
        is_done: false,
        has_usage: false,
    };

    // Split on newlines to find `data:` lines.
    for line in frame.split(|b| *b == b'\n') {
        let payload = strip_data_prefix(line);
        let trimmed = trim_ascii(payload);

        if trimmed.starts_with(b"[DONE]") && trimmed.len() == 6 {
            result.is_done = true;
            continue;
        }

        // Try to parse as JSON.
        let json_val = match serde_json::from_slice::<Value>(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Inspect choices[0].delta
        if let Some(choices) = json_val.get("choices").and_then(|c| c.as_array()) {
            if let Some(first) = choices.first() {
                if let Some(delta) = first.get("delta") {
                    // delta.content
                    match delta.get("content") {
                        Some(Value::String(s)) if !s.is_empty() => result.has_content = true,
                        Some(Value::Array(a)) if !a.is_empty() => result.has_content = true,
                        Some(Value::Object(o)) if !o.is_empty() => result.has_content = true,
                        _ => {}
                    }

                    // delta.tool_calls
                    match delta.get("tool_calls") {
                        Some(Value::Array(a)) if !a.is_empty() => result.has_tool_calls = true,
                        _ => {}
                    }
                }

                // choices[0].finish_reason
                if result.finish_reason.is_none() {
                    if let Some(Value::String(s)) = first.get("finish_reason") {
                        result.finish_reason = Some(s.clone());
                    }
                }
            }
        }

        // Top-level usage
        match json_val.get("usage") {
            Some(Value::Object(o)) if !o.is_empty() => result.has_usage = true,
            _ => {}
        }
    }

    result
}

/// Strip a leading `data:` (optionally followed by one space) from a line.
fn strip_data_prefix(line: &[u8]) -> &[u8] {
    const PREFIX: &[u8] = b"data:";
    const PREFIX_SPACE: &[u8] = b"data: ";

    if line.starts_with(PREFIX_SPACE) {
        &line[PREFIX_SPACE.len()..]
    } else if line.starts_with(PREFIX) {
        &line[PREFIX.len()..]
    } else {
        // Not a data line — return empty slice so it'll fail JSON parse harmlessly.
        &[]
    }
}

/// Trim leading and trailing ASCII whitespace from a byte slice.
fn trim_ascii(b: &[u8]) -> &[u8] {
    let start = b.iter().position(|c| !is_ascii_ws(*c)).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !is_ascii_ws(*c)).map_or(0, |i| i + 1);
    if start >= end {
        &[]
    } else {
        &b[start..end]
    }
}

fn is_ascii_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r')
}

// ---------------------------------------------------------------------------
// SseFrameParser
// ---------------------------------------------------------------------------

/// Accumulates raw bytes from a backend SSE stream and yields complete
/// events delimited by `\n\n` (or `\r\n\r\n`).
///
/// Each yielded event includes its trailing delimiter bytes so callers can
/// forward raw bytes verbatim.
pub struct SseFrameParser {
    buffer: Vec<u8>,
}

impl Default for SseFrameParser {
    fn default() -> Self {
        Self::new()
    }
}


impl SseFrameParser {
    pub fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    /// Feed a chunk of bytes. Returns complete events (each including its
    /// trailing `\n\n` or `\r\n\r\n` delimiter).
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(chunk);
        self.extract_events()
    }

    /// Consume the parser and return any remaining incomplete event as a
    /// single trailing element.  Returns an empty vec if the buffer is empty.
    pub fn finish(self) -> Vec<Vec<u8>> {
        if self.buffer.is_empty() {
            Vec::new()
        } else {
            vec![self.buffer]
        }
    }

    fn extract_events(&mut self) -> Vec<Vec<u8>> {
        let mut events = Vec::new();

        loop {
            // Look for the next delimiter in the buffer.
            let pos = self.find_delimiter();
            match pos {
                Some(end) => {
                    // `end` is the index of the first byte after the delimiter.
                    let event = self.buffer[..end].to_vec();
                    events.push(event);
                    // Drain consumed bytes.
                    self.buffer.drain(..end);
                }
                None => break,
            }
        }

        events
    }

    /// Find the byte offset immediately after the next `\n\n` or `\r\n\r\n`
    /// delimiter, or `None` if no complete delimiter exists.
    fn find_delimiter(&self) -> Option<usize> {
        let buf = &self.buffer;
        if buf.len() < 2 {
            return None;
        }

        // Search for `\n\n` (byte 0x0a, 0x0a) or `\r\n\r\n` (bytes 0x0d,0x0a,0x0d,0x0a).
        // We need to find the *first* delimiter occurrence.

        let mut search_start = 0;
        while search_start + 1 < buf.len() {
            // Check for \r\n\r\n first (longer match, must be checked before \n\n at same position).
            if buf[search_start] == b'\r'
                && buf.get(search_start + 1) == Some(&b'\n')
                && buf.get(search_start + 2) == Some(&b'\r')
                && buf.get(search_start + 3) == Some(&b'\n')
            {
                return Some(search_start + 4);
            }
            // Check for \n\n.
            if buf[search_start] == b'\n' && buf[search_start + 1] == b'\n' {
                return Some(search_start + 2);
            }
            search_start += 1;
        }

        None
    }
}

// ---------------------------------------------------------------------------
// llama.cpp intake error classification (plan 007, task 04)
// ---------------------------------------------------------------------------

/// Classification of a backend error body for transient re-forwarding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlamacppErrorClass {
    /// The body is not a recognizable llama.cpp intake error (vLLM-shaped
    /// error, malformed JSON, or missing the token counts). Never retried.
    NotLlamacpp,
    /// llama.cpp `exceed_context_size_error` with
    /// `n_prompt_tokens >= n_ctx` — the prompt cannot fit slot capacity;
    /// permanent, passes through unchanged.
    Permanent,
    /// llama.cpp `exceed_context_size_error` with
    /// `n_prompt_tokens < n_ctx` — the prompt fits slot capacity once the
    /// backend's transient state (e.g. restart) clears; safe to re-forward
    /// the identical body.
    Transient,
}

/// Classify a backend error body for transient re-forwarding.
///
/// Only llama.cpp's `error.type == "exceed_context_size_error"` (with both
/// `n_prompt_tokens` and `n_ctx` present as integers) is classified; the
/// `type` field is the reliable discriminator (the message string is never
/// matched). Everything else — including malformed JSON — is
/// [`LlamacppErrorClass::NotLlamacpp`].
pub fn classify_llamacpp_error(body: &[u8]) -> LlamacppErrorClass {
    let value = match serde_json::from_slice::<Value>(body) {
        Ok(v) => v,
        Err(_) => return LlamacppErrorClass::NotLlamacpp,
    };
    let error = match value.get("error").and_then(|e| e.as_object()) {
        Some(e) => e,
        None => return LlamacppErrorClass::NotLlamacpp,
    };
    // The `type` field is the reliable discriminator.
    if error.get("type").and_then(|t| t.as_str()) != Some("exceed_context_size_error") {
        return LlamacppErrorClass::NotLlamacpp;
    }
    match (
        error.get("n_prompt_tokens").and_then(|v| v.as_i64()),
        error.get("n_ctx").and_then(|v| v.as_i64()),
    ) {
        (Some(prompt_tokens), Some(ctx)) => {
            if prompt_tokens < ctx {
                LlamacppErrorClass::Transient
            } else {
                LlamacppErrorClass::Permanent
            }
        }
        // Missing or non-integer counts: unclassifiable.
        _ => LlamacppErrorClass::NotLlamacpp,
    }
}

/// Returns `true` for network failures that are expected to clear on their
/// own while the request is still re-forwardable — i.e. the backend is
/// restarting or briefly unreachable under live traffic:
///
/// - connect-phase failures (connection refused, DNS, connect timeout),
/// - a connection that died mid-flight, surfaced in the error's source
///   chain as an io error: reset / aborted / refused / broken pipe /
///   unexpected EOF.
///
/// Conservative: request-build, redirect, and body/decode errors are never
/// transient, and response-phase timeouts are not network errors at all.
pub fn is_transient_network_error(e: &reqwest::Error) -> bool {
    // Connect-phase failure (refused, DNS, connect timeout).
    if e.is_connect() {
        return true;
    }
    // Walk the source chain for an io::Error with a "the connection died"
    // kind (reqwest/hyper surface connection-level failures this way).
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
    while let Some(s) = source {
        if let Some(io_err) = s.downcast_ref::<std::io::Error>() {
            if matches!(
                io_err.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::UnexpectedEof
            ) {
                return true;
            }
        }
        source = s.source();
    }
    false
}

/// Bounded exponential backoff for a 1-based transient re-forward attempt:
/// `min(backoff_start * 2^(attempt-1), backoff_max)`.
pub fn transient_backoff(attempt: u32, policy: &TransientRetry) -> std::time::Duration {
    let mut delay = policy.backoff_start;
    for _ in 0..attempt.saturating_sub(1) {
        delay = delay.saturating_mul(2);
        if delay >= policy.backoff_max {
            return policy.backoff_max;
        }
    }
    delay
}

/// Re-issue a request to the backend, propagating the send error.
///
/// Behaves like the premature-stop reissue helper (clones headers, strips
/// `Content-Length` so the transport recomputes it from the body, applies
/// the optional per-attempt timeout), but returns the actual
/// [`reqwest::Error`] — or [`ProxyError::Timeout`] on attempt timeout — so
/// the caller can classify transient network failures.
pub async fn send_retry_request_with_error(
    client: &reqwest::Client,
    method: &axum::http::Method,
    backend_url: &url::Url,
    headers: &axum::http::HeaderMap,
    body: bytes::Bytes,
    request_timeout: Option<std::time::Duration>,
) -> Result<reqwest::Response, ProxyError> {
    let mut rh = headers.clone();
    rh.remove(axum::http::header::CONTENT_LENGTH);
    let mut rb = client
        .request(method.clone(), backend_url.clone())
        .body(body);
    for (n, v) in rh.iter() {
        rb = rb.header(n, v);
    }
    match request_timeout {
        Some(t) => match tokio::time::timeout(t, rb.send()).await {
            Ok(x) => x.map_err(ProxyError::Network),
            Err(_) => Err(ProxyError::Timeout),
        },
        None => rb.send().await.map_err(ProxyError::Network),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- is_premature_stop tests ----

    #[test]
    fn premature_stop_with_no_content_and_no_tool_calls() {
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "content": null,
                    "tool_calls": []
                }
            }]
        });
        assert!(is_premature_stop(&body));
    }

    #[test]
    fn premature_stop_with_empty_string_content() {
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "content": "",
                    "tool_calls": []
                }
            }]
        });
        assert!(is_premature_stop(&body));
    }

    #[test]
    fn premature_stop_with_absent_content_and_absent_tool_calls() {
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {}
            }]
        });
        assert!(is_premature_stop(&body));
    }

    #[test]
    fn not_premature_with_non_empty_content() {
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "content": "Hello!",
                    "tool_calls": []
                }
            }]
        });
        assert!(!is_premature_stop(&body));
    }

    #[test]
    fn not_premature_with_non_empty_tool_calls() {
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "content": null,
                    "tool_calls": [{"name": "search"}]
                }
            }]
        });
        assert!(!is_premature_stop(&body));
    }

    #[test]
    fn premature_with_finish_reason_length() {
        // finish_reason "length" with no content/tool_calls is degenerate
        // (token-capped mid-thinking): treated as premature so the retry
        // path fires with a bumped temperature.
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "length",
                "message": {
                    "content": null,
                    "tool_calls": []
                }
            }]
        });
        assert!(is_premature_stop(&body));
    }

    #[test]
    fn not_premature_with_finish_reason_tool_calls() {
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": null,
                    "tool_calls": []
                }
            }]
        });
        assert!(!is_premature_stop(&body));
    }

    #[test]
    fn not_premature_with_missing_choices() {
        let body = serde_json::json!({
            "id": "chatcmpl-123"
        });
        assert!(!is_premature_stop(&body));
    }

    #[test]
    fn not_premature_with_empty_choices() {
        let body = serde_json::json!({
            "choices": []
        });
        assert!(!is_premature_stop(&body));
    }

    #[test]
    fn not_premature_with_content_as_non_empty_array() {
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "content": [{"text": "hello"}],
                    "tool_calls": []
                }
            }]
        });
        assert!(!is_premature_stop(&body));
    }

    #[test]
    fn not_premature_with_tool_calls_as_non_empty_array() {
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "content": null,
                    "tool_calls": [{"function": {"name": "search"}}]
                }
            }]
        });
        assert!(!is_premature_stop(&body));
    }

    #[test]
    fn not_premature_with_finish_reason_error() {
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "error",
                "message": {}
            }]
        });
        assert!(!is_premature_stop(&body));
    }

    #[test]
    fn not_premature_with_finish_reason_abort() {
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "abort",
                "message": {}
            }]
        });
        assert!(!is_premature_stop(&body));
    }

    // ---- bump_temperature tests ----

    #[test]
    fn bump_temperature_from_body_temp() {
        let body = serde_json::json!({
            "model": "local",
            "temperature": 0.5,
            "messages": [{"role": "user", "content": "hi"}],
            "stream_options": {"include_usage": true}
        });
        let policy = RetryPolicy {
            max_retries: 2,
            temperature_step: 0.3,
            max_temperature: 1.5,
            default_temperature: 0.0,
            ..Default::default()
        };
        let result = bump_temperature(&body, 1, &policy);
        // base=0.5, attempt=1, step=0.3 => 0.8
        assert_eq!(result["temperature"].as_f64(), Some(0.8));
        // Other fields preserved
        assert_eq!(result["model"], "local");
        assert!(result["messages"].is_array());
        assert!(result["stream_options"].is_object());
    }

    #[test]
    fn bump_temperature_falls_back_to_default() {
        let body = serde_json::json!({
            "model": "local",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let policy = RetryPolicy {
            max_retries: 2,
            temperature_step: 0.3,
            max_temperature: 1.5,
            default_temperature: 0.1,
            ..Default::default()
        };
        let result = bump_temperature(&body, 1, &policy);
        // base=0.1 (default), attempt=1, step=0.3 => 0.4
        assert_eq!(result["temperature"].as_f64(), Some(0.4));
    }

    #[test]
    fn bump_temperature_clamped_at_max() {
        let body = serde_json::json!({
            "temperature": 1.4,
            "model": "local"
        });
        let policy = RetryPolicy {
            max_retries: 2,
            temperature_step: 0.3,
            max_temperature: 1.5,
            default_temperature: 0.0,
            ..Default::default()
        };
        let result = bump_temperature(&body, 1, &policy);
        // base=1.4, attempt=1, step=0.3 => 1.7, clamped to 1.5
        assert_eq!(result["temperature"].as_f64(), Some(1.5));
    }

    #[test]
    fn bump_temperature_attempt_2() {
        let body = serde_json::json!({
            "temperature": 0.0,
            "model": "local"
        });
        let policy = RetryPolicy {
            max_retries: 2,
            temperature_step: 0.3,
            max_temperature: 1.5,
            default_temperature: 0.0,
            ..Default::default()
        };
        let result = bump_temperature(&body, 2, &policy);
        // base=0.0, attempt=2, step=0.3 => 0.6
        assert_eq!(result["temperature"].as_f64(), Some(0.6));
    }

    #[test]
    fn bump_temperature_preserves_other_fields() {
        let body = serde_json::json!({
            "model": "local",
            "temperature": 0.5,
            "messages": [{"role": "user", "content": "hello world"}],
            "stream": true,
            "stream_options": {"include_usage": true},
            "max_tokens": 100
        });
        let policy = RetryPolicy::default();
        let result = bump_temperature(&body, 1, &policy);
        // messages preserved
        assert_eq!(result["messages"][0]["role"], "user");
        assert_eq!(result["messages"][0]["content"], "hello world");
        // stream_options preserved
        assert_eq!(result["stream_options"]["include_usage"], true);
        // stream preserved
        assert_eq!(result["stream"], true);
        // max_tokens preserved
        assert_eq!(result["max_tokens"], 100);
    }

    // ---- classify_frame tests ----

    #[test]
    fn classify_frame_content_delta() {
        let frame = b"data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n";
        let result = classify_frame(frame);
        assert!(result.has_content);
        assert!(!result.has_tool_calls);
        assert!(!result.is_done);
        assert!(result.finish_reason.is_none());
        assert!(!result.has_usage);
    }

    #[test]
    fn classify_frame_tool_calls_delta() {
        let frame =
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"1\"}]}}]}\n\n";
        let result = classify_frame(frame);
        assert!(!result.has_content);
        assert!(result.has_tool_calls);
        assert!(!result.is_done);
    }

    #[test]
    fn classify_frame_finish_reason_chunk() {
        let frame =
            b"data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n";
        let result = classify_frame(frame);
        assert_eq!(result.finish_reason, Some("stop".to_string()));
    }

    #[test]
    fn classify_frame_done() {
        let frame = b"data: [DONE]\n\n";
        let result = classify_frame(frame);
        assert!(result.is_done);
        assert!(!result.has_content);
        assert!(!result.has_tool_calls);
        assert!(result.finish_reason.is_none());
    }

    #[test]
    fn classify_frame_usage_chunk() {
        let frame =
            b"data: {\"usage\":{\"completion_tokens\":100}}\n\n";
        let result = classify_frame(frame);
        assert!(result.has_usage);
        assert!(!result.has_content);
        assert!(!result.has_tool_calls);
    }

    #[test]
    fn classify_frame_non_json_data_line() {
        let frame = b"data: this is not json\n\n";
        let result = classify_frame(frame);
        assert!(!result.has_content);
        assert!(!result.has_tool_calls);
        assert!(result.finish_reason.is_none());
        assert!(!result.is_done);
        assert!(!result.has_usage);
    }

    #[test]
    fn classify_frame_multiple_data_lines() {
        let frame = b"data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\ndata: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n";
        let result = classify_frame(frame);
        assert!(result.has_content);
        assert_eq!(result.finish_reason, Some("stop".to_string()));
    }

    #[test]
    fn classify_frame_empty_content_string() {
        let frame = b"data: {\"choices\":[{\"delta\":{\"content\":\"\"}}]}\n\n";
        let result = classify_frame(frame);
        assert!(!result.has_content);
    }

    #[test]
    fn classify_frame_empty_tool_calls_array() {
        let frame = b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[]}}]}\n\n";
        let result = classify_frame(frame);
        assert!(!result.has_tool_calls);
    }

    #[test]
    fn classify_frame_null_usage() {
        let frame = b"data: {\"usage\":null}\n\n";
        let result = classify_frame(frame);
        assert!(!result.has_usage);
    }

    // ---- SseFrameParser tests ----

    #[test]
    fn parser_one_complete_event() {
        let mut parser = SseFrameParser::new();
        let events = parser.feed(b"data: {\"content\":\"hi\"}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], b"data: {\"content\":\"hi\"}\n\n");
    }

    #[test]
    fn parser_multiple_events_in_one_chunk() {
        let mut parser = SseFrameParser::new();
        let events = parser.feed(b"data: first\n\ndata: second\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], b"data: first\n\n");
        assert_eq!(events[1], b"data: second\n\n");
    }

    #[test]
    fn parser_event_split_across_three_feeds() {
        let mut parser = SseFrameParser::new();
        let mut events = Vec::new();

        events.extend(parser.feed(b"data: hello"));
        assert!(events.is_empty(), "nothing complete yet");

        events.extend(parser.feed(b" world\n\n"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], b"data: hello world\n\n");
    }

    #[test]
    fn parser_chunk_ending_mid_delimiter_single_newline() {
        let mut parser = SseFrameParser::new();

        // Feed up to but not including the full delimiter (single \n).
        let events = parser.feed(b"data: test\n");
        assert!(events.is_empty(), "single \\n should not yield an event");

        // Feed the second \n to complete the delimiter.
        let events = parser.feed(b"\ndata: next\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], b"data: test\n\n");
        assert_eq!(events[1], b"data: next\n\n");
    }

    #[test]
    fn parser_finish_returns_leftover() {
        let mut parser = SseFrameParser::new();
        let events = parser.feed(b"data: complete\n\nincomplete");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], b"data: complete\n\n");

        let leftover = parser.finish();
        assert_eq!(leftover.len(), 1);
        assert_eq!(leftover[0], b"incomplete");
    }

    #[test]
    fn parser_finish_empty_when_no_buffer() {
        let mut parser = SseFrameParser::new();
        let _ = parser.feed(b"data: done\n\n");
        let leftover = parser.finish();
        assert!(leftover.is_empty());
    }

    #[test]
    fn parser_crlf_delimiter() {
        let mut parser = SseFrameParser::new();
        let events = parser.feed(b"data: hello\r\n\r\ndata: world\r\n\r\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], b"data: hello\r\n\r\n");
        assert_eq!(events[1], b"data: world\r\n\r\n");
    }

    #[test]
    fn parser_empty_feed() {
        let mut parser = SseFrameParser::new();
        let events = parser.feed(&[]);
        assert!(events.is_empty());
    }

    #[test]
    fn parser_multiple_events_with_crlf() {
        let mut parser = SseFrameParser::new();
        let events = parser.feed(b"data: a\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], b"data: a\r\n\r\n");
    }


    #[test]
    fn classify_frame_whitespace_data_payload_does_not_panic() {
        // All-whitespace data: payload must not panic; returns all-false pass-through.
        for frame in [b"data:  \n\n" as &[u8], b"data:\t\n\n", b"data: \r\n\n", b"data:\n\n"] {
            let c = classify_frame(frame);
            assert!(!c.has_content);
            assert!(!c.has_tool_calls);
            assert!(c.finish_reason.is_none());
            assert!(!c.is_done);
            assert!(!c.has_usage);
        }
    }

    #[test]
    fn classify_frame_content_delta_as_non_empty_array() {
        let frame = b"data: {\"choices\":[{\"delta\":{\"content\":[\"hello\"]}}]}\n\n";
        let c = classify_frame(frame);
        assert!(c.has_content);
        assert!(!c.has_tool_calls);
        assert!(!c.is_done);
    }

    #[test]
    fn bump_temperature_non_finite_default_does_not_panic() {
        let policy = RetryPolicy { default_temperature: f64::NAN, ..Default::default() };
        let body = serde_json::json!({"model": "x", "messages": []});
        // Must not panic; temperature becomes null (or finite), other fields preserved.
        let out = bump_temperature(&body, 1, &policy);
        assert_eq!(out["messages"], body["messages"]);
    }

    // ---- classify_llamacpp_error tests ----

    fn ctx_error_body(prompt: i64, ctx: i64) -> Vec<u8> {
        serde_json::json!({
            "error": {
                "code": 400,
                "type": "exceed_context_size_error",
                "message": "Prompt is too long",
                "n_prompt_tokens": prompt,
                "n_ctx": ctx
            }
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn classify_llamacpp_transient_when_prompt_fits() {
        assert_eq!(
            classify_llamacpp_error(&ctx_error_body(100_000, 262_144)),
            LlamacppErrorClass::Transient
        );
    }

    #[test]
    fn classify_llamacpp_permanent_when_prompt_exceeds() {
        assert_eq!(
            classify_llamacpp_error(&ctx_error_body(300_000, 262_144)),
            LlamacppErrorClass::Permanent
        );
    }

    #[test]
    fn classify_llamacpp_permanent_when_prompt_equals_ctx() {
        assert_eq!(
            classify_llamacpp_error(&ctx_error_body(262_144, 262_144)),
            LlamacppErrorClass::Permanent
        );
    }

    #[test]
    fn classify_llamacpp_other_type_is_not_llamacpp() {
        let body = br#"{"error":{"type":"some_other_error","message":"bad"}}"#;
        assert_eq!(classify_llamacpp_error(body), LlamacppErrorClass::NotLlamacpp);
    }

    #[test]
    fn classify_llamacpp_malformed_json_is_not_llamacpp() {
        assert_eq!(classify_llamacpp_error(b"not json"), LlamacppErrorClass::NotLlamacpp);
        assert_eq!(classify_llamacpp_error(b""), LlamacppErrorClass::NotLlamacpp);
    }

    #[test]
    fn classify_llamacpp_missing_counts_is_not_llamacpp() {
        let body =
            br#"{"error":{"code":400,"type":"exceed_context_size_error","message":"x"}}"#;
        assert_eq!(classify_llamacpp_error(body), LlamacppErrorClass::NotLlamacpp);
    }

    // ---- transient_backoff tests ----

    #[test]
    fn transient_backoff_exponential_and_capped() {
        let policy = TransientRetry {
            max_attempts: 5,
            backoff_start: std::time::Duration::from_millis(100),
            backoff_max: std::time::Duration::from_millis(350),
        };
        assert_eq!(transient_backoff(1, &policy), std::time::Duration::from_millis(100));
        assert_eq!(transient_backoff(2, &policy), std::time::Duration::from_millis(200));
        assert_eq!(transient_backoff(3, &policy), std::time::Duration::from_millis(350));
        assert_eq!(transient_backoff(10, &policy), std::time::Duration::from_millis(350));
    }

    // ---- is_transient_network_error tests ----

    #[tokio::test]
    async fn transient_network_error_connection_refused_is_transient() {
        // Bind and drop a listener to obtain a closed local port.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let err = reqwest::Client::new()
            .get(format!("http://{}/", addr))
            .send()
            .await
            .unwrap_err();
        assert!(
            is_transient_network_error(&err),
            "connection refused should be transient (got: {})",
            err
        );
    }

    #[tokio::test]
    async fn non_network_error_is_not_transient() {
        // Unsupported scheme: a request-build error, never transient.
        let err = reqwest::Client::new()
            .get("ftp://127.0.0.1:1/")
            .send()
            .await
            .unwrap_err();
        assert!(
            !is_transient_network_error(&err),
            "request-build error should not be transient"
        );
    }
}
