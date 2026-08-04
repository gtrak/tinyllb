use tinyllb::config::ContextPolicy;
use tinyllb::context::estimator::TokenEstimator;
use tinyllb::context::reconcile::reconcile;
use tinyllb::context::segment::find_turn_boundaries;
use tinyllb::context::store::{SqliteStore, TranscriptStore};
use serde_json::json;
use serde_json::Value;
use std::time::Duration;

fn msg(role: &str, content: &str) -> Value {
    json!({ "role": role, "content": content })
}

fn msg_tool(role: &str, content: &str, tool_calls: Value) -> Value {
    json!({ "role": role, "content": content, "tool_calls": tool_calls })
}

fn test_policy(threshold: usize) -> ContextPolicy {
    ContextPolicy {
        enabled: true,
        compress_threshold: threshold,
        head_keep_turns: 3,
        live_keep_turns: 6,
        compress_chunk_turns: 8,
        ..Default::default()
    }
}

async fn open_store() -> SqliteStore {
    SqliteStore::open(":memory:").await.expect("open in-memory store")
}

#[tokio::test]
async fn test_tool_call_grouping() {
    let store = open_store().await;
    let estimator = TokenEstimator::new(None);
    let config = test_policy(100_000);

    // Messages with tool_calls and tool role messages.
    // The tool_calls message (assistant) + tool response should be grouped
    // into the same logical turn boundary.
    let messages: Vec<Value> = vec![
        msg("system", "you are helpful"),
        msg_tool(
            "assistant",
            "",
            json!([
                {
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "add",
                        "arguments": "{\"a\":1,\"b\":2}"
                    }
                }
            ]),
        ),
        msg("tool", "42"),
        msg("user", "thanks for the calculation"),
        msg("assistant", "you're welcome"),
        msg("user", "can you do another one?"),
        msg("assistant", "sure, result is 5"),
    ];

    let result =
        reconcile("tool-flow", &messages, &store, &estimator, &config)
            .await
            .unwrap();

    // Verify the tool_calls message is preserved in forwarded messages.
    assert_eq!(result.forwarded_messages.len(), messages.len());
    assert!(
        result.forwarded_messages[1]
            .get("tool_calls")
            .is_some(),
        "tool_calls message should be preserved",
    );

    // Verify tool response is in forwarded messages.
    assert_eq!(
        result.forwarded_messages[2]["role"]
            .as_str()
            .expect("role is string"),
        "tool",
        "tool role message should be forwarded",
    );

    // Check turn boundaries: system + assistant(tool_calls) + tool + user + assistant + user + assistant
    // User messages at indices 3, 5 → boundaries = [0, 5].
    let boundaries = find_turn_boundaries(&messages);
    assert_eq!(
        boundaries.len(),
        2,
        "should have 2 turns (turn 0: system+tool_call+tool+user+assistant, turn 1: user+assistant)",
    );

    // Append same messages — should not diverge.
    let result2 =
        reconcile("tool-flow", &messages, &store, &estimator, &config)
            .await
            .unwrap();
    assert!(
        !result2.transcript_reset,
        "should not detect divergence on re-send",
    );
}

#[tokio::test]
async fn test_multimodal_content() {
    let store = open_store().await;
    let estimator = TokenEstimator::new(None);
    let config = test_policy(100_000);

    // Message with multimodal content (array of text + image parts).
    let messages: Vec<Value> = vec![
        msg("system", "you are a vision assistant"),
        json!({
            "role": "user",
            "content": [
                { "type": "text", "text": "describe this image" },
                { "type": "image_url", "image_url": { "url": "http://example.com/img.png" } }
            ]
        }),
        msg("assistant", "This is a description of the image"),
        msg("user", "what color is it?"),
        msg("assistant", "it is blue"),
    ];

    let result =
        reconcile("multimodal-flow", &messages, &store, &estimator, &config)
            .await
            .unwrap();

    // Forwarded messages should include the multimodal content unchanged.
    assert_eq!(result.forwarded_messages.len(), messages.len());

    // Verify the multimodal message is preserved with its array content.
    let user_msg = &result.forwarded_messages[1];
    let content_is_array = user_msg
        .get("content")
        .map(|c| c.is_array())
        .unwrap_or(false);
    assert!(
        content_is_array,
        "multimodal content should be preserved as array",
    );

    // Estimate tokens — should still work with multimodal content.
    assert!(
        result.total_est_tokens > 0,
        "total_est_tokens should be positive even with multimodal",
    );

    // Verify the estimator correctly handles multimodal content.
    let est = estimator.estimate_messages(&messages);
    assert!(est > 0, "estimator should produce positive count for multimodal");
}

#[tokio::test]
async fn test_large_conversation() {
    let store = open_store().await;
    let estimator = TokenEstimator::new(None);
    let config = test_policy(100_000);

    // Build a conversation with 50 turns (no system message for clean turn counting).
    let start = std::time::Instant::now();
    let mut messages: Vec<Value> = Vec::new();
    for i in 1..=50 {
        messages.push(msg(
            "user",
            &format!("question number {} of the large conversation", i),
        ));
        messages.push(msg(
            "assistant",
            &format!(
                "answer to question {} with some additional context for the large conversation",
                i
            ),
        ));
    }

    let result =
        reconcile("large-flow", &messages, &store, &estimator, &config)
            .await
            .unwrap();

    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "reconciliation of 50 turns should complete within 5 seconds (took {:?})",
        elapsed,
    );

    // Verify forwarded messages contain all messages (no compression yet).
    assert_eq!(result.forwarded_messages.len(), messages.len());
    assert!(result.total_est_tokens > 0);

    // Verify head + live segments were created.
    let transcript = store.load_transcript("large-flow").await.unwrap();
    assert_eq!(transcript.segments.len(), 2); // Head + Live

    // With head_keep_turns=3 and 50 turns:
    // head = first 3 turns = 6 messages.
    // live = remaining 47 turns = 94 messages.
    let head = &transcript.segments[0];
    assert_eq!(
        head.raw_messages.len(),
        6,
        "head should have 3 turns (6 messages)",
    );
    let live = &transcript.segments[1];
    assert_eq!(
        live.raw_messages.len(),
        94,
        "live should have 47 turns (94 messages)",
    );
}

#[tokio::test]
async fn test_incoming_missing_messages() {
    // When the incoming request body has no `messages` field,
    // the proxy should forward the body unchanged.
    // In the reconcile layer, an empty messages array returns early.
    let store = open_store().await;
    let estimator = TokenEstimator::new(None);
    let config = test_policy(100_000);

    // Pass empty messages — reconcile should return early.
    let result = reconcile("missing-flow", &[], &store, &estimator, &config)
        .await
        .unwrap();

    assert!(
        result.forwarded_messages.is_empty(),
        "empty messages should produce empty forwarded_messages",
    );
    assert_eq!(result.total_est_tokens, 0);
    assert_eq!(result.total_raw_est_tokens, 0);
    assert!(!result.needs_compression);
    assert!(result.compress_turn_range.is_none());
    assert!(!result.transcript_reset);

    // Verify no transcript was created in the store.
    let transcript = store.load_transcript("missing-flow").await.unwrap();
    assert!(
        transcript.segments.is_empty(),
        "no transcript should be created for empty messages",
    );
}

#[tokio::test]
async fn test_empty_conversation() {
    // First request with an empty messages array.
    // No transcript should be created; forwarded as-is.
    let store = open_store().await;
    let estimator = TokenEstimator::new(None);
    let config = test_policy(100_000);

    let empty_messages: Vec<Value> = vec![];

    let result =
        reconcile("empty-flow", &empty_messages, &store, &estimator, &config)
            .await
            .unwrap();

    assert!(
        result.forwarded_messages.is_empty(),
        "forwarded_messages should be empty",
    );
    assert_eq!(result.total_est_tokens, 0);
    assert!(!result.needs_compression);

    // No transcript in store.
    let transcript = store.load_transcript("empty-flow").await.unwrap();
    assert!(
        transcript.segments.is_empty(),
        "no transcript created for empty conversation",
    );
}
