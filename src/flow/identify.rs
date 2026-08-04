use axum::http::HeaderMap;
use bytes::Bytes;
use uuid::Uuid;

use super::{FlowId, ResolvedFlow, resolve_priority_token};

/// Resolve a request's flow identity and priority override.
///
/// Resolution order (highest to lowest precedence):
/// 1. `X-LLM-Flow-ID` request header (explicit override).
/// 2. Harness session headers (case-insensitive; empty values fall through):
///    - `x-claude-code-session-id` (Claude Code)
///    - `x-session-id` (opencode, pi, de-facto standard)
///    - `x-session-affinity` (opencode, pi)
///    - `x-client-request-id` (pi, Codex)
///    - `session_id` (pi/Codex Responses, best-effort)
/// 3. `metadata.flow_id` in the JSON body (best-effort; skipped for
///    non-JSON bodies).
/// 4. Auto-generated ephemeral ID (`ephemeral-{UUIDv4}`).
///
/// Additionally parses `X-LLM-Priority` header (case-insensitive):
/// - `interactive` / `agent` / `background` → `priority_override = Some(class)`
/// - `auto` → `unset_override = true` (clear any prior pin, resume heuristic)
/// - Unknown value → logged as warning, treated as absent (`None`, `unset=false`)
/// - Absent header → `priority_override = None`, `unset_override = false`
///
/// Returns a `ResolvedFlow` containing the resolved flow ID and priority override state.
/// For the full priority heuristic and tuning guide, see
/// `docs/plans/001-tinyllb/PRIORITY.md`.
// @lat: [[flow#Flow Identification]]
pub fn resolve(headers: &HeaderMap, body: &Bytes) -> ResolvedFlow {
    // 1. Check the X-LLM-Flow-ID header first (highest precedence).
    let mut flow_id: Option<FlowId> = extract_flow_id_from_header(headers);

    // 2. Try harness session headers (Claude Code, opencode, pi, standard).
    if flow_id.is_none() {
        flow_id = extract_flow_id_from_session_headers(headers);
    }

    // 3. Try to extract from JSON body metadata.flow_id.
    //    This is best-effort: if the body isn't valid JSON or doesn't have
    //    the metadata field, fall through to ephemeral.
    if flow_id.is_none() {
        flow_id = extract_flow_id_from_body(body);
    }

    // 4. Generate an ephemeral flow ID if nothing else resolved.
    let flow_id: FlowId = flow_id.unwrap_or_else(generate_ephemeral_id);

    // Parse X-LLM-Priority header (case-insensitive).
    let (priority_override, unset_override) = match headers
        .get("x-llm-priority")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        None => (None, false), // absent header: no state change
        Some(val) => {
            if val.eq_ignore_ascii_case("auto") {
                (None, true) // explicit resume-heuristic request
            } else if let Some(class) = resolve_priority_token(val) {
                (Some(class), false)
            } else {
                // Unknown value: log warning, proceed as if absent.
                tracing::warn!(value = %val, "unknown X-LLM-Priority value, ignoring");
                (None, false)
            }
        }
    };

    ResolvedFlow {
        flow_id,
        priority_override,
        unset_override,
    }
}

/// Extract a flow ID from the `X-LLM-Flow-ID` header.
///
/// Returns `None` if the header is absent or empty.
fn extract_flow_id_from_header(headers: &HeaderMap) -> Option<FlowId> {
    headers
        .get("x-llm-flow-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(FlowId::new)
}

/// Extract a stable session ID from harness session headers.
///
/// Order (highest to lowest precedence):
/// 1. `x-claude-code-session-id` (Claude Code)
/// 2. `x-session-id` (de-facto standard: opencode, pi, vLLM, Anthropic-proxy)
/// 3. `x-session-affinity` (opencode, pi)
/// 4. `x-client-request-id` (pi / Codex OpenAI-compatible paths)
/// 5. `session_id` (pi / Codex Responses wire header; underscore form, best-effort)
///
/// Returns `None` when none of the headers are present or all are
/// empty — the caller falls through to the body and ephemeral paths.
fn extract_flow_id_from_session_headers(headers: &HeaderMap) -> Option<FlowId> {
    for name in [
        "x-claude-code-session-id",
        "x-session-id",
        "x-session-affinity",
        "x-client-request-id",
        "session_id",
    ] {
        if let Some(value) = headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .filter(|s| !s.trim().is_empty())
        {
            return Some(FlowId::new(value.trim().to_string()));
        }
    }
    None
}

/// Extract a flow ID from the JSON body's `metadata.flow_id` field.
///
/// Returns `None` if:
/// - The body is not valid JSON.
/// - The JSON does not contain `metadata.flow_id`.
/// - The `flow_id` value is not a non-empty string.
fn extract_flow_id_from_body(body: &Bytes) -> Option<FlowId> {
    if body.is_empty() {
        return None;
    }

    let value = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let metadata = value.get("metadata")?;
    let flow_id = metadata.get("flow_id")?.as_str()?;

    if flow_id.is_empty() {
        None
    } else {
        Some(FlowId::new(flow_id.to_string()))
    }
}

/// Generate an ephemeral flow ID using a UUIDv4.
fn generate_ephemeral_id() -> FlowId {
    let uuid = Uuid::new_v4().to_string();
    FlowId::new(format!("ephemeral-{uuid}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn header_flow_id_is_extracted() {
        let mut headers = HeaderMap::new();
        headers.insert("x-llm-flow-id", HeaderValue::from_static("agent-1"));
        let body = Bytes::from_static(b"{}");

        let id = resolve(&headers, &body).flow_id;
        assert_eq!(id.to_string(), "agent-1");
    }

    #[test]
    fn empty_header_falls_through() {
        let mut headers = HeaderMap::new();
        headers.insert("x-llm-flow-id", HeaderValue::from_static(""));
        let body = Bytes::from_static(b"{}");

        let id = resolve(&headers, &body).flow_id;
        assert!(id.is_ephemeral());
    }

    #[test]
    fn metadata_flow_id_is_extracted() {
        let headers = HeaderMap::new();
        let body =
            Bytes::from(r#"{"metadata":{"flow_id":"agent-2"},"model":"llama-2"}"#.as_bytes());

        let id = resolve(&headers, &body).flow_id;
        assert_eq!(id.to_string(), "agent-2");
    }

    #[test]
    fn header_takes_precedence_over_metadata() {
        let mut headers = HeaderMap::new();
        headers.insert("x-llm-flow-id", HeaderValue::from_static("header-wins"));
        let body = Bytes::from(
            r#"{"metadata":{"flow_id":"metadata-losing"},"model":"llama-2"}"#.as_bytes(),
        );

        let id = resolve(&headers, &body).flow_id;
        assert_eq!(id.to_string(), "header-wins");
    }

    #[test]
    fn non_json_body_falls_through_to_ephemeral() {
        let headers = HeaderMap::new();
        let body = Bytes::from_static(b"not-json-binary-data");

        let id = resolve(&headers, &body).flow_id;
        assert!(id.is_ephemeral());
    }

    #[test]
    fn empty_body_falls_through_to_ephemeral() {
        let headers = HeaderMap::new();
        let body = Bytes::new();

        let id = resolve(&headers, &body).flow_id;
        assert!(id.is_ephemeral());
    }

    #[test]
    fn ephemeral_ids_are_unique() {
        let id1 = generate_ephemeral_id();
        let id2 = generate_ephemeral_id();
        assert!(id1.is_ephemeral());
        assert!(id2.is_ephemeral());
        assert_ne!(id1, id2);
    }

    #[test]
    fn ephemeral_metric_label_is_aggregated() {
        let id = generate_ephemeral_id();
        assert_eq!(id.metric_label(), "ephemeral");
    }

    #[test]
    fn named_flow_metric_label_is_exact() {
        let id = FlowId::new("my-agent");
        assert_eq!(id.metric_label(), "my-agent");
    }

    // ─── Session header tests (Plan 003) ───

    #[test]
    fn claude_code_session_id_is_extracted() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-claude-code-session-id",
            HeaderValue::from_static("ses_abc"),
        );
        let body = Bytes::from_static(b"{}");

        let id = resolve(&headers, &body).flow_id;
        assert_eq!(id.to_string(), "ses_abc");
        assert!(!id.is_ephemeral());
    }

    #[test]
    fn standard_x_session_id_is_extracted() {
        let mut headers = HeaderMap::new();
        headers.insert("x-session-id", HeaderValue::from_static("ses_xyz"));
        let body = Bytes::from_static(b"{}");

        let id = resolve(&headers, &body).flow_id;
        assert_eq!(id.to_string(), "ses_xyz");
    }

    #[test]
    fn x_session_affinity_is_extracted() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-session-affinity",
            HeaderValue::from_static("ses_42"),
        );
        let body = Bytes::from_static(b"{}");

        let id = resolve(&headers, &body).flow_id;
        assert_eq!(id.to_string(), "ses_42");
    }

    #[test]
    fn x_client_request_id_is_extracted() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-client-request-id",
            HeaderValue::from_static("abc-uuid"),
        );
        let body = Bytes::from_static(b"{}");

        let id = resolve(&headers, &body).flow_id;
        assert_eq!(id.to_string(), "abc-uuid");
    }

    #[test]
    fn underscore_session_id_is_extracted() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "session_id",
            HeaderValue::from_static("codex-s-1"),
        );
        let body = Bytes::from_static(b"{}");

        let id = resolve(&headers, &body).flow_id;
        assert_eq!(id.to_string(), "codex-s-1");
    }

    #[test]
    fn x_llm_flow_id_overrides_harness_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-llm-flow-id", HeaderValue::from_static("my-flow"));
        headers.insert(
            "x-claude-code-session-id",
            HeaderValue::from_static("ses_other"),
        );
        let body = Bytes::from_static(b"{}");

        let id = resolve(&headers, &body).flow_id;
        assert_eq!(id.to_string(), "my-flow");
    }

    #[test]
    fn harness_headers_are_case_insensitive() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Session-Id",
            HeaderValue::from_static("Ses_1"),
        );
        let body = Bytes::from_static(b"{}");

        let id = resolve(&headers, &body).flow_id;
        assert_eq!(id.to_string(), "Ses_1");
    }

    #[test]
    fn empty_harness_headers_fall_through_to_body() {
        let mut headers = HeaderMap::new();
        headers.insert("x-session-id", HeaderValue::from_static(""));
        let body =
            Bytes::from(r#"{"metadata":{"flow_id":"agent-2"},"model":"llama-2"}"#.as_bytes());

        let id = resolve(&headers, &body).flow_id;
        assert_eq!(id.to_string(), "agent-2");
    }

    #[test]
    fn empty_harness_headers_fall_through_to_ephemeral() {
        let mut headers = HeaderMap::new();
        headers.insert("x-session-id", HeaderValue::from_static(""));
        let body = Bytes::from_static(b"{}");

        let id = resolve(&headers, &body).flow_id;
        assert!(id.is_ephemeral());
    }

    #[test]
    fn session_headers_beat_metadata_flow_id() {
        let mut headers = HeaderMap::new();
        headers.insert("x-session-id", HeaderValue::from_static("ses_a"));
        let body = Bytes::from(
            r#"{"metadata":{"flow_id":"agent-b"},"model":"llama-2"}"#.as_bytes(),
        );

        let id = resolve(&headers, &body).flow_id;
        assert_eq!(id.to_string(), "ses_a");
    }

    #[test]
    fn claude_code_has_priority_over_x_session_id() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-claude-code-session-id",
            HeaderValue::from_static("claude-wins"),
        );
        headers.insert(
            "x-session-id",
            HeaderValue::from_static("session-loses"),
        );
        let body = Bytes::from_static(b"{}");

        let id = resolve(&headers, &body).flow_id;
        assert_eq!(id.to_string(), "claude-wins");
    }

    #[test]
    fn whitespace_harness_headers_fall_through() {
        let mut headers = HeaderMap::new();
        headers.insert("x-session-id", HeaderValue::from_static("   "));
        let body = Bytes::from_static(b"{}");

        let id = resolve(&headers, &body).flow_id;
        assert!(id.is_ephemeral());
    }
}
