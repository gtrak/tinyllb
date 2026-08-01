use axum::http::HeaderMap;
use bytes::Bytes;
use uuid::Uuid;

use super::FlowId;

/// Resolve a `FlowId` from a request.
///
/// Resolution order (highest to lowest precedence):
/// 1. `X-LLM-Flow-ID` request header (if present and non-empty).
/// 2. `metadata.flow_id` in the JSON body (best-effort; skipped for
///    non-JSON bodies).
/// 3. Auto-generated ephemeral ID (`ephemeral-{UUIDv4}`).
///
/// Returns the resolved `FlowId`.
pub fn resolve(headers: &HeaderMap, body: &Bytes) -> FlowId {
    // 1. Check the X-LLM-Flow-ID header first (highest precedence).
    if let Some(header_id) = extract_flow_id_from_header(headers) {
        return header_id;
    }

    // 2. Try to extract from JSON body metadata.flow_id.
    //    This is best-effort: if the body isn't valid JSON or doesn't have
    //    the metadata field, fall through to ephemeral.
    if let Some(metadata_id) = extract_flow_id_from_body(body) {
        return metadata_id;
    }

    // 3. Generate an ephemeral flow ID.
    generate_ephemeral_id()
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

        let id = resolve(&headers, &body);
        assert_eq!(id.to_string(), "agent-1");
    }

    #[test]
    fn empty_header_falls_through() {
        let mut headers = HeaderMap::new();
        headers.insert("x-llm-flow-id", HeaderValue::from_static(""));
        let body = Bytes::from_static(b"{}");

        let id = resolve(&headers, &body);
        assert!(id.is_ephemeral());
    }

    #[test]
    fn metadata_flow_id_is_extracted() {
        let headers = HeaderMap::new();
        let body =
            Bytes::from(r#"{"metadata":{"flow_id":"agent-2"},"model":"llama-2"}"#.as_bytes());

        let id = resolve(&headers, &body);
        assert_eq!(id.to_string(), "agent-2");
    }

    #[test]
    fn header_takes_precedence_over_metadata() {
        let mut headers = HeaderMap::new();
        headers.insert("x-llm-flow-id", HeaderValue::from_static("header-wins"));
        let body = Bytes::from(
            r#"{"metadata":{"flow_id":"metadata-losing"},"model":"llama-2"}"#.as_bytes(),
        );

        let id = resolve(&headers, &body);
        assert_eq!(id.to_string(), "header-wins");
    }

    #[test]
    fn non_json_body_falls_through_to_ephemeral() {
        let headers = HeaderMap::new();
        let body = Bytes::from_static(b"not-json-binary-data");

        let id = resolve(&headers, &body);
        assert!(id.is_ephemeral());
    }

    #[test]
    fn empty_body_falls_through_to_ephemeral() {
        let headers = HeaderMap::new();
        let body = Bytes::new();

        let id = resolve(&headers, &body);
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
}
