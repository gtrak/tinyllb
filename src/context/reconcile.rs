//! Reconciliation: match incoming messages against stored transcript,
//! detect new turns, handle conversation resets, and determine compression needs.

use crate::config::ContextPolicy;
use crate::context::estimator::TokenEstimator;
use crate::context::segment::{Segment, SegmentKind, Transcript, find_turn_boundaries, split_messages_at_turns};
use crate::context::store::{TranscriptMeta, TranscriptStore};
use anyhow::Context;
use chrono::Utc;
use serde_json::Value;

/// Result of reconciling an incoming request against a stored transcript.
pub struct ReconcileResult {
    /// Messages to forward to the backend (may differ from incoming after
    /// reconciliation with compressed segments).
    pub forwarded_messages: Vec<Value>,
    /// Estimated tokens of forwarded messages (post-compression).
    pub total_est_tokens: usize,
    /// Estimated tokens of the full conversation (pre-compression).
    pub total_raw_est_tokens: usize,
    /// Whether compression should be triggered.
    pub needs_compression: bool,
    /// If compression needed, the turn range within the Live segment to compress
    /// (inclusive start, exclusive end). `None` means nothing to compress.
    pub compress_turn_range: Option<(usize, usize)>,
    /// `true` if a conversation reset was detected and the old transcript was
    /// replaced.
    pub transcript_reset: bool,
}

/// Compare two messages by role and content only.
///
/// Ignores extra metadata fields so that a client re-adding `name`,
/// `function_call`, etc. does not cause false divergence.
fn messages_equal(a: &Value, b: &Value) -> bool {
    let a_role = a.get("role").and_then(|v| v.as_str());
    let b_role = b.get("role").and_then(|v| v.as_str());
    if a_role != b_role {
        return false;
    }
    let a_content = a.get("content").and_then(|v| v.as_str());
    let b_content = b.get("content").and_then(|v| v.as_str());
    if a_content != b_content {
        return false;
    }
    true
}

/// Return messages in `incoming` that are not already in `stored_live`.
///
/// Uses prefix matching: the longest common prefix of `incoming` and
/// `stored_live` is skipped; everything after is considered new.
fn find_new_turns(incoming: &[Value], stored_live: &[Value]) -> Vec<Value> {
    if stored_live.is_empty() {
        return incoming.to_vec();
    }
    if incoming.len() < stored_live.len() {
        return Vec::new();
    }
    let mut prefix_len = 0;
    for i in 0..stored_live.len().min(incoming.len()) {
        if messages_equal(&incoming[i], &stored_live[i]) {
            prefix_len += 1;
        } else {
            break;
        }
    }
    incoming[prefix_len..].to_vec()
}

/// Reconcile an incoming request's `messages` array against the stored
/// transcript for `flow_id`.
///
/// Detects new turns, handles divergent history (client reset), updates the
/// Live segment, and determines whether compression is needed.
pub async fn reconcile(
    flow_id: &str,
    incoming_messages: &[Value],
    store: &dyn TranscriptStore,
    estimator: &TokenEstimator,
    config: &ContextPolicy,
) -> anyhow::Result<ReconcileResult> {
    // Step 1: Empty messages → early return.
    if incoming_messages.is_empty() {
        return Ok(ReconcileResult {
            forwarded_messages: Vec::new(),
            total_est_tokens: 0,
            total_raw_est_tokens: 0,
            needs_compression: false,
            compress_turn_range: None,
            transcript_reset: false,
        });
    }

    // Step 2: Load existing transcript.
    let transcript = store.load_transcript(flow_id).await?;

    if transcript.segments.is_empty() {
        // Step 3: No existing transcript — create new.
        create_new_transcript(
            flow_id,
            incoming_messages,
            store,
            estimator,
            config,
            false,
        )
        .await
    } else {
        // Step 4: Transcript exists — diff against stored Live.
        reconcile_existing(
            flow_id,
            incoming_messages,
            &transcript,
            store,
            estimator,
            config,
        )
        .await
    }
}

/// Helper: create a brand-new transcript from incoming messages.
async fn create_new_transcript(
    flow_id: &str,
    incoming: &[Value],
    store: &dyn TranscriptStore,
    estimator: &TokenEstimator,
    config: &ContextPolicy,
    transcript_reset: bool,
) -> anyhow::Result<ReconcileResult> {
    let (head_msgs, middle_msgs, live_msgs) =
        split_messages_at_turns(incoming, config.head_keep_turns, config.live_keep_turns);

    // The Live segment absorbs the middle (everything not in head).
    let live_all: Vec<Value> = {
        let mut combined = middle_msgs;
        combined.extend(live_msgs);
        combined
    };

    let now = Utc::now();
    let mut segment_idx = 0i32;

    // Capture values from head_msgs before it gets moved into the segment.
    let head_len = head_msgs.len();
    let head_turns = find_turn_boundaries(&head_msgs).len() as i32;

    // Create Head segment if non-empty.
    if head_len > 0 {
        let head_est = estimator.estimate_messages(&head_msgs) as i32;
        let head_seg = Segment {
            flow_id: flow_id.to_string(),
            segment_idx: 0,
            kind: SegmentKind::Head,
            raw_messages: head_msgs,
            summary_message: None,
            msg_start_idx: 0,
            msg_end_idx: head_len as i32,
            est_tokens: head_est,
            raw_est_tokens: head_est,
            created_at: now,
        };
        store
            .save_segment(&head_seg)
            .await
            .context("failed to save head segment")?;
        segment_idx += 1;
    }

    // Always create a Live segment (even if empty) so reconciliation on subsequent
    // requests has a Live segment to diff against.
    let live_est = estimator.estimate_messages(&live_all) as i32;
    let live_turns = find_turn_boundaries(&live_all).len() as i32;
    let live_seg = Segment {
        flow_id: flow_id.to_string(),
        segment_idx,
        kind: SegmentKind::Live,
        raw_messages: live_all,
        summary_message: None,
        msg_start_idx: head_len as i32,
        msg_end_idx: incoming.len() as i32,
        est_tokens: live_est,
        raw_est_tokens: live_est,
        created_at: now,
    };
    store
        .save_segment(&live_seg)
        .await
        .context("failed to save live segment")?;

    // Forwarded = incoming (fresh transcript, nothing compressed).
    let forwarded = incoming.to_vec();
    let total_est = estimator.estimate_messages(&forwarded);
    let total_raw = total_est; // No compression yet.

    // Compression trigger check (live is empty when all turns go to head,
    // so live_turns == 0 → compress_turn_range == None).
    let (needs_compression, compress_turn_range) =
        check_compression_with_turns(live_turns as usize, total_est, config);

    let meta = TranscriptMeta {
        flow_id: flow_id.to_string(),
        head_turns,
        live_turns,
        compressed_count: 0,
        last_compressed_turn: 0,
        total_est_tokens: total_est as i32,
        total_raw_est_tokens: total_raw as i32,
        updated_at: now.to_rfc3339(),
    };
    store
        .upsert_meta(&meta)
        .await
        .context("failed to upsert meta")?;

    Ok(ReconcileResult {
        forwarded_messages: forwarded,
        total_est_tokens: total_est,
        total_raw_est_tokens: total_raw,
        needs_compression,
        compress_turn_range,
        transcript_reset,
    })
}

/// Helper: reconcile when a transcript already exists.
async fn reconcile_existing(
    flow_id: &str,
    incoming: &[Value],
    transcript: &Transcript,
    store: &dyn TranscriptStore,
    estimator: &TokenEstimator,
    config: &ContextPolicy,
) -> anyhow::Result<ReconcileResult> {
    let stored_head = transcript.head_segment();
    let stored_live = transcript.live_segment().map(|s| &s.raw_messages);
    let stored_head_raw = stored_head.map(|s| &s.raw_messages);

    // Divergence detection: if both stored_head and incoming are non-empty and
    // the first message's role+content differ → conversation reset.
    if let Some(head_msgs) = stored_head_raw {
        if !head_msgs.is_empty() && !incoming.is_empty() && !messages_equal(&incoming[0], &head_msgs[0]) {
            // Client reset: delete and recreate.
            store
                .delete_transcript(flow_id)
                .await
                .context("failed to delete transcript on reset")?;
            return create_new_transcript(
                flow_id,
                incoming,
                store,
                estimator,
                config,
                true,
            )
            .await;
        }
    }

    // No divergence — find new messages.
    // Build the full stored content (head + live) to compare against.
    let empty: Vec<Value> = Vec::new();
    let stored_head_msgs: &Vec<Value> = stored_head_raw.unwrap_or(&empty);
    let stored_live_msgs: &Vec<Value> = stored_live.unwrap_or(&empty);
    let stored_all: Vec<Value> = stored_head_msgs.iter().chain(stored_live_msgs.iter()).cloned().collect();

    let new_turns = find_new_turns(incoming, &stored_all);

    if !new_turns.is_empty() {
        // Build new live = stored_live + new_turns (Live contains only non-head messages).
        let new_live: Vec<Value> = stored_live_msgs.iter().chain(new_turns.iter()).cloned().collect();
        let live_est = estimator.estimate_messages(&new_live) as i32;
        let live_raw_est = live_est;
        store
            .update_live_segment(flow_id, &new_live, live_est, live_raw_est)
            .await
            .context("failed to update live segment")?;
    }

    // Recompute: reload transcript.
    let updated_transcript = store
        .load_transcript(flow_id)
        .await
        .context("failed to reload transcript after update")?;

    let forwarded = updated_transcript.forwarded_messages();
    let total_est = updated_transcript.total_est_tokens();
    let total_raw = updated_transcript.total_raw_est_tokens();

    // Compression trigger check (use the live segment raw messages).
    let empty_vec: Vec<Value> = Vec::new();
    let live_raw: &Vec<Value> = updated_transcript
        .live_segment()
        .map(|s| &s.raw_messages)
        .unwrap_or(&empty_vec);
    let (needs_compression, compress_turn_range) =
        check_compression(live_raw, total_est, config);

    // Update meta.
    let head_turns = updated_transcript
        .head_segment()
        .map(|s| find_turn_boundaries(&s.raw_messages).len() as i32)
        .unwrap_or(0);
    let live_turns = updated_transcript
        .live_segment()
        .map(|s| find_turn_boundaries(&s.raw_messages).len() as i32)
        .unwrap_or(0);
    let compressed_count = updated_transcript
        .compressed_segments()
        .count() as i32;
    let last_compressed_turn = updated_transcript
        .compressed_segments()
        .last()
        .map(|s| s.msg_end_idx)
        .unwrap_or(0);
    let now = Utc::now();
    let meta = TranscriptMeta {
        flow_id: flow_id.to_string(),
        head_turns,
        live_turns,
        compressed_count,
        last_compressed_turn,
        total_est_tokens: total_est as i32,
        total_raw_est_tokens: total_raw as i32,
        updated_at: now.to_rfc3339(),
    };
    store
        .upsert_meta(&meta)
        .await
        .context("failed to upsert meta")?;

    Ok(ReconcileResult {
        forwarded_messages: forwarded,
        total_est_tokens: total_est,
        total_raw_est_tokens: total_raw,
        needs_compression,
        compress_turn_range,
        transcript_reset: false,
    })
}

/// Determine whether compression should be triggered and, if so, which turn
/// range in the Live segment to compress.
fn check_compression(
    live_raw: &[Value],
    total_est: usize,
    config: &ContextPolicy,
) -> (bool, Option<(usize, usize)>) {
    let live_turns = find_turn_boundaries(live_raw).len();
    check_compression_with_turns(live_turns, total_est, config)
}

/// Same as [`check_compression`] but accepts a pre-computed turn count.
fn check_compression_with_turns(
    live_turns: usize,
    total_est: usize,
    config: &ContextPolicy,
) -> (bool, Option<(usize, usize)>) {
    if total_est > config.compress_threshold {
        if live_turns <= config.live_keep_turns {
            (true, None)
        } else {
            let compressible = live_turns - config.live_keep_turns;
            let end = config.compress_chunk_turns.min(compressible);
            (true, Some((0, end)))
        }
    } else {
        (false, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::store::SqliteStore;

    fn msg(role: &str, content: &str) -> Value {
        serde_json::json!({"role": role, "content": content})
    }

    fn msg_tool(role: &str, content: &str, tool_calls: Value) -> Value {
        serde_json::json!({"role": role, "content": content, "tool_calls": tool_calls})
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

    fn test_policy_custom(
        threshold: usize,
        head_turns: usize,
        live_turns: usize,
        chunk_turns: usize,
    ) -> ContextPolicy {
        ContextPolicy {
            enabled: true,
            compress_threshold: threshold,
            head_keep_turns: head_turns,
            live_keep_turns: live_turns,
            compress_chunk_turns: chunk_turns,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_new_transcript() {
        let store = SqliteStore::open(":memory:").await.unwrap();
        let estimator = TokenEstimator::new(None);
        let config = test_policy(100_000);

        // First request: system + 2 turns
        let messages: Vec<Value> = vec![
            msg("system", "you are helpful"),
            msg("user", "question 1"),
            msg("assistant", "answer 1"),
            msg("user", "question 2"),
            msg("assistant", "answer 2"),
        ];

        let result = reconcile("flow-1", &messages, &store, &estimator, &config).await.unwrap();

        // Forwarded == incoming for a fresh transcript.
        assert_eq!(result.forwarded_messages, messages);
        assert!(!result.transcript_reset);
        assert!(!result.needs_compression);
        assert!(result.compress_turn_range.is_none());
        assert!(result.total_est_tokens > 0);
        assert_eq!(result.total_est_tokens, result.total_raw_est_tokens);

        // Verify segments were created.
        let transcript = store.load_transcript("flow-1").await.unwrap();
        assert_eq!(transcript.segments.len(), 2);
        assert_eq!(transcript.segments[0].kind, SegmentKind::Head);
        assert_eq!(transcript.segments[1].kind, SegmentKind::Live);
    }

    #[tokio::test]
    async fn test_append_new_turns() {
        let store = SqliteStore::open(":memory:").await.unwrap();
        let estimator = TokenEstimator::new(None);
        let config = test_policy(100_000);

        // First request: system + 1 turn
        let first: Vec<Value> = vec![
            msg("system", "you are helpful"),
            msg("user", "question 1"),
            msg("assistant", "answer 1"),
        ];
        let r1 = reconcile("flow-a", &first, &store, &estimator, &config).await.unwrap();
        assert!(!r1.transcript_reset);

        // Second request: same + new turn
        let second: Vec<Value> = vec![
            msg("system", "you are helpful"),
            msg("user", "question 1"),
            msg("assistant", "answer 1"),
            msg("user", "question 2"),
            msg("assistant", "answer 2"),
        ];
        let r2 = reconcile("flow-a", &second, &store, &estimator, &config).await.unwrap();
        assert!(!r2.transcript_reset);
        // Forwarded includes all messages (head + extended live).
        assert_eq!(r2.forwarded_messages, second);

        // Verify live segment was updated.
        let transcript = store.load_transcript("flow-a").await.unwrap();
        let live = transcript.live_segment().unwrap();
        // With head_keep_turns=3, the first turn goes to head (3 msgs).
        // Live contains only the non-head messages from the second request (2 msgs).
        assert_eq!(live.raw_messages.len(), 2);
    }

    #[tokio::test]
    async fn test_no_new_turns() {
        let store = SqliteStore::open(":memory:").await.unwrap();
        let estimator = TokenEstimator::new(None);
        let config = test_policy(100_000);

        let messages: Vec<Value> = vec![
            msg("system", "you are helpful"),
            msg("user", "question 1"),
            msg("assistant", "answer 1"),
        ];

        // First request.
        let r1 = reconcile("flow-b", &messages, &store, &estimator, &config).await.unwrap();
        assert!(!r1.transcript_reset);

        // Second request: identical messages (no new turns).
        let r2 = reconcile("flow-b", &messages, &store, &estimator, &config).await.unwrap();
        assert!(!r2.transcript_reset);
        assert_eq!(r2.forwarded_messages, messages);

        // With head_keep_turns=3 and only 1 turn, live is empty (all in head).
        let transcript = store.load_transcript("flow-b").await.unwrap();
        let live = transcript.live_segment().unwrap();
        assert!(live.raw_messages.is_empty());
        assert_eq!(transcript.total_est_tokens(), r2.total_est_tokens);
    }

    #[tokio::test]
    async fn test_conversation_reset() {
        let store = SqliteStore::open(":memory:").await.unwrap();
        let estimator = TokenEstimator::new(None);
        let config = test_policy(100_000);

        // First request.
        let first: Vec<Value> = vec![
            msg("system", "you are helpful"),
            msg("user", "question 1"),
            msg("assistant", "answer 1"),
        ];
        reconcile("flow-c", &first, &store, &estimator, &config).await.unwrap();

        // Second request: different first user message (client reset).
        let second: Vec<Value> = vec![
            msg("system", "you are a pirate"),
            msg("user", "arrrr"),
            msg("assistant", "yo ho ho"),
        ];
        let result =
            reconcile("flow-c", &second, &store, &estimator, &config).await.unwrap();

        assert!(result.transcript_reset);
        assert_eq!(result.forwarded_messages, second);

        // Transcript should have new segments matching the second conversation.
        let transcript = store.load_transcript("flow-c").await.unwrap();
        assert_eq!(transcript.segments.len(), 2); // Head + Live
    }

    #[tokio::test]
    async fn test_triggers_compression() {
        let store = SqliteStore::open(":memory:").await.unwrap();
        let estimator = TokenEstimator::new(None);
        // Very low threshold to trigger compression.
        let config = test_policy(1);

        // Build a conversation with 10 turns.
        let mut messages: Vec<Value> = vec![msg("system", "init")];
        for i in 1..=10 {
            messages.push(msg("user", &format!("question {}", i)));
            messages.push(
                msg("assistant", &format!("answer number {} with lots of text to push token count higher and higher until we cross the threshold", i)),
            );
        }

        let result = reconcile("flow-d", &messages, &store, &estimator, &config).await.unwrap();

        // With threshold=1, any non-empty conversation triggers compression.
        assert!(result.needs_compression);
        // 10 turns, head=3, live=7. Live has 7 turns > live_keep_turns(6).
        // compressible = 7 - 6 = 1. end = min(8, 1) = 1.
        assert!(result.compress_turn_range.is_some());
        let (start, end) = result.compress_turn_range.unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 1);
    }

    #[tokio::test]
    async fn test_no_compression_under_threshold() {
        let store = SqliteStore::open(":memory:").await.unwrap();
        let estimator = TokenEstimator::new(None);
        let config = test_policy(999_999);

        let messages: Vec<Value> = vec![
            msg("system", "you are helpful"),
            msg("user", "hello"),
            msg("assistant", "hi"),
        ];

        let result = reconcile("flow-e", &messages, &store, &estimator, &config).await.unwrap();

        assert!(!result.needs_compression);
        assert!(result.compress_turn_range.is_none());
    }

    #[tokio::test]
    async fn test_tool_calls_in_turns() {
        let store = SqliteStore::open(":memory:").await.unwrap();
        let estimator = TokenEstimator::new(None);
        let config = test_policy(100_000);

        let messages: Vec<Value> = vec![
            msg("system", "you are helpful"),
            msg_tool("assistant", "", serde_json::json!([
                {"id": "call_1", "type": "function", "function": {"name": "add", "arguments": "{}"}}
            ])),
            msg("tool", "42"),
            msg("user", "thanks for the calculation"),
            msg("assistant", "you're welcome"),
        ];

        let result = reconcile("flow-f", &messages, &store, &estimator, &config).await.unwrap();
        assert!(!result.transcript_reset);
        assert_eq!(result.forwarded_messages.len(), messages.len());

        // Verify the tool_calls message is preserved.
        assert!(result.forwarded_messages[1].get("tool_calls").is_some());

        // Append same messages — should not diverge (tool role is different
        // from user, so divergence check only looks at first message).
        let result2 =
            reconcile("flow-f", &messages, &store, &estimator, &config).await.unwrap();
        assert!(!result2.transcript_reset);
    }

    #[tokio::test]
    async fn test_empty_messages() {
        let store = SqliteStore::open(":memory:").await.unwrap();
        let estimator = TokenEstimator::new(None);
        let config = test_policy(100_000);

        let result =
            reconcile("flow-g", &[], &store, &estimator, &config).await.unwrap();

        assert!(result.forwarded_messages.is_empty());
        assert_eq!(result.total_est_tokens, 0);
        assert_eq!(result.total_raw_est_tokens, 0);
        assert!(!result.needs_compression);
        assert!(result.compress_turn_range.is_none());
        assert!(!result.transcript_reset);
    }

    #[tokio::test]
    async fn test_incoming_shorter_than_stored() {
        let store = SqliteStore::open(":memory:").await.unwrap();
        let estimator = TokenEstimator::new(None);
        let config = test_policy(100_000);

        // First request: 4 messages.
        let first: Vec<Value> = vec![
            msg("system", "init"),
            msg("user", "q1"),
            msg("assistant", "a1"),
            msg("user", "q2"),
        ];
        reconcile("flow-h", &first, &store, &estimator, &config).await.unwrap();

        // Second request: only 2 messages (shorter than stored live).
        let shorter: Vec<Value> = vec![
            msg("system", "init"),
            msg("user", "q1"),
        ];
        let result =
            reconcile("flow-h", &shorter, &store, &estimator, &config).await.unwrap();

        // No divergence (first msg matches stored head first msg).
        assert!(!result.transcript_reset);
        // Incoming is shorter — no new turns, transcript unchanged.
        // Forwarded still contains the stored head content.
        assert!(result.total_est_tokens > 0);
        assert_eq!(result.forwarded_messages, first);
    }

    #[tokio::test]
    async fn test_head_boundary() {
        let store = SqliteStore::open(":memory:").await.unwrap();
        let estimator = TokenEstimator::new(None);
        // head_keep_turns=2, so first 2 turns go to head, rest to live.
        let config = test_policy_custom(100_000, 2, 6, 8);

        // 5 turns (no system, so 5 user messages = 5 turns).
        let mut messages: Vec<Value> = Vec::new();
        for i in 1..=5 {
            messages.push(msg("user", &format!("question {}", i)));
            messages.push(msg("assistant", &format!("answer {}", i)));
        }
        // 10 messages, 5 turns.

        let result = reconcile("flow-i", &messages, &store, &estimator, &config).await.unwrap();
        assert!(!result.transcript_reset);

        // Verify segments.
        let transcript = store.load_transcript("flow-i").await.unwrap();
        assert_eq!(transcript.segments.len(), 2);
        let head = &transcript.segments[0];
        let live = &transcript.segments[1];

        // Head should have 2 turns (4 messages).
        assert_eq!(head.kind, SegmentKind::Head);
        assert_eq!(head.raw_messages.len(), 4);
        assert_eq!(find_turn_boundaries(&head.raw_messages).len(), 2);

        // Live should have 3 turns (6 messages).
        assert_eq!(live.kind, SegmentKind::Live);
        assert_eq!(live.raw_messages.len(), 6);
        assert_eq!(find_turn_boundaries(&live.raw_messages).len(), 3);

        // Forwarded == incoming.
        assert_eq!(result.forwarded_messages, messages);
    }
}
