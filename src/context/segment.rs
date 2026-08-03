//! Segment-based transcript model for context compression.
//!
//! Each flow's conversation is modeled as an ordered list of immutable segments:
//! `[Head] [Compressed_1..n] [Live]`. Segements are append-only once created;
//! only the Live segment may be replaced (e.g. during reconciliation).

use chrono::{DateTime, Utc};
use serde_json::Value;

/// The role of a segment within a transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SegmentKind {
    /// First N turns, kept verbatim.
    Head,
    /// Summary message replacing original turns.
    Compressed,
    /// Most recent turns, kept verbatim.
    Live,
}

/// An immutable segment of a conversation transcript.
///
/// Head and Compressed segments are immutable once created. The Live segment
/// is mutable — it grows with new messages and is replaced during
/// reconciliation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Segment {
    /// Identifies which conversation flow this segment belongs to.
    pub flow_id: String,
    /// Index within the transcript's segment list.
    pub segment_idx: i32,
    /// Whether this segment is Head, Compressed, or Live.
    pub kind: SegmentKind,
    /// Original raw messages for this segment's turn range.
    ///
    /// For Head/Live: the actual messages forwarded to vLLM.
    /// For Compressed: the original messages that were compressed away
    /// (kept for audit / potential re-compression).
    pub raw_messages: Vec<Value>,
    /// For Compressed only: the summary message sent to vLLM.
    ///
    /// Head/Live forward `raw_messages` directly; this field is `None`.
    pub summary_message: Option<Value>,
    /// Start message index (inclusive) in the original conversation.
    pub msg_start_idx: i32,
    /// End message index (exclusive) in the original conversation.
    pub msg_end_idx: i32,
    /// Estimated tokens of what's forwarded (raw for head/live, summary for compressed).
    pub est_tokens: i32,
    /// Estimated tokens of the original raw messages (for compressed: what raw
    /// would've cost; for head/live: same as est_tokens).
    pub raw_est_tokens: i32,
    /// When this segment was created.
    pub created_at: DateTime<Utc>,
}

impl Segment {
    /// Return the messages that should be forwarded to vLLM for this segment.
    ///
    /// Head/Live → references into `raw_messages`.
    /// Compressed → reference to the single `summary_message`.
    pub fn forwarded_messages(&self) -> Vec<&Value> {
        match self.kind {
            SegmentKind::Head | SegmentKind::Live => self.raw_messages.iter().collect(),
            SegmentKind::Compressed => self.summary_message.iter().collect(),
        }
    }

    /// Whether this segment is immutable (cannot be modified in-place).
    ///
    /// Head and Compressed segments are immutable. Live segments may grow
    /// and be replaced.
    pub fn is_immutable(&self) -> bool {
        match self.kind {
            SegmentKind::Head | SegmentKind::Compressed => true,
            SegmentKind::Live => false,
        }
    }
}

/// An ordered list of segments representing a complete conversation transcript.
#[derive(Debug, Clone)]
pub struct Transcript {
    /// Identifies the flow this transcript belongs to.
    pub flow_id: String,
    /// Ordered segments: `[Head] [Compressed..] [Live]`.
    pub segments: Vec<Segment>,
}

impl Transcript {
    /// Concatenate the forwarded messages from all segments in order.
    ///
    /// Returns the complete `messages` array to send to vLLM.
    pub fn forwarded_messages(&self) -> Vec<Value> {
        let mut result = Vec::new();
        for segment in &self.segments {
            for msg in segment.forwarded_messages() {
                result.push(msg.clone());
            }
        }
        result
    }

    /// Sum of `est_tokens` across all segments (forwarded token budget).
    pub fn total_est_tokens(&self) -> usize {
        self.segments.iter().map(|s| s.est_tokens as usize).sum()
    }

    /// Sum of `raw_est_tokens` across all segments (what the full conversation would cost).
    pub fn total_raw_est_tokens(&self) -> usize {
        self.segments.iter().map(|s| s.raw_est_tokens as usize).sum()
    }

    /// Token savings achieved by compression.
    ///
    /// `total_raw_est_tokens - total_est_tokens`. Zero or positive.
    pub fn compression_savings(&self) -> usize {
        self.total_raw_est_tokens().saturating_sub(self.total_est_tokens())
    }

    /// Return the first Head segment, if present.
    pub fn head_segment(&self) -> Option<&Segment> {
        self.segments.iter().find(|s| s.kind == SegmentKind::Head)
    }

    /// Return the Live segment (conventionally the last segment).
    pub fn live_segment(&self) -> Option<&Segment> {
        self.segments.iter().rev().find(|s| s.kind == SegmentKind::Live)
    }

    /// Iterate over all Compressed segments.
    pub fn compressed_segments(&self) -> impl Iterator<Item = &Segment> {
        self.segments.iter().filter(|s| s.kind == SegmentKind::Compressed)
    }

    /// Count turns by counting user-role messages across all forwarded messages.
    ///
    /// If there are no user messages but there are messages, count as 1 turn.
    /// If empty, 0 turns.
    pub fn turn_count(&self) -> usize {
        let msgs = self.forwarded_messages();
        let user_count = msgs
            .iter()
            .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
            .count();
        if user_count > 0 {
            user_count
        } else if !msgs.is_empty() {
            1
        } else {
            0
        }
    }
}

/// Find the message index where each turn starts.
///
/// A turn = user message + all following non-user messages until the next user
/// message. A leading system message attaches to the first turn.
///
/// Algorithm:
/// - Empty messages → empty vec.
/// - `boundaries[0] = 0` always (if non-empty).
/// - For each subsequent user-role message (2nd, 3rd, …), append its index.
/// - If there are no user messages but messages exist → `[0]`.
///
/// **Example:**
/// `[system, user1, assistant1, user2, assistant2]`
/// User messages at indices 1, 3 → boundaries = `[0, 3]`.
/// (Turn 0 = indices 0..3, Turn 1 = indices 3..5)
pub fn find_turn_boundaries(messages: &[Value]) -> Vec<usize> {
    if messages.is_empty() {
        return Vec::new();
    }

    let mut user_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .map(|(i, _)| i)
        .collect();

    if user_indices.is_empty() {
        // No user messages → entire array is one turn.
        return vec![0usize];
    }

    // First turn starts at index 0. Subsequent turns start at each user message
    // after the first (2nd, 3rd, … user messages).
    let _first_user = user_indices.remove(0);
    let mut boundaries = vec![0usize];
    // If the first user message is not at index 0, there may be a system
    // message, but the first turn still starts at 0.
    // Subsequent user messages (2nd, 3rd, ...) start new turns.
    for &idx in &user_indices {
        boundaries.push(idx);
    }
    // The first user itself might be at index > 0 (system prefix), but it
    // still belongs to turn 0 which starts at 0.

    boundaries
}

/// Split a message array into head, middle, and live parts by turn boundaries.
///
/// Returns `(head_msgs, middle_msgs, live_msgs)`.
///
/// - `head_msgs`: messages belonging to the first `head_turns` turns.
/// - `live_msgs`: messages belonging to the last `live_turns` turns.
/// - `middle_msgs`: everything in between (candidates for compression).
///
/// If total turns <= `head_turns` + `live_turns`, `middle_msgs` is empty.
/// Head takes precedence; live shrinks to fill the remainder.
///
/// **Example:** 10 turns, head=3, live=6 → head has turns 0..3,
/// live has turns 7..10, middle has turn 3..7 (1 turn).
pub fn split_messages_at_turns(
    messages: &[Value],
    head_turns: usize,
    live_turns: usize,
) -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    if messages.is_empty() {
        return (Vec::new(), Vec::new(), Vec::new());
    }

    let boundaries = find_turn_boundaries(messages);
    let total_turns = boundaries.len();

    if total_turns <= head_turns + live_turns {
        // Not enough turns for separate head + live; head takes precedence,
        // live gets whatever remains.
        let actual_head = total_turns.min(head_turns);
        let actual_live = total_turns.saturating_sub(actual_head);

        // head turns: 0..actual_head
        let head_end = if actual_head < total_turns {
            boundaries[actual_head]
        } else {
            messages.len()
        };

        // live turns: actual_head..total_turns (remaining turns for live)
        let live_start = if actual_head + actual_live < total_turns {
            boundaries[actual_head + actual_live]
        } else {
            head_end // no middle, live starts right after head
        };

        let head_msgs = messages[..head_end].to_vec();
        let middle_msgs = Vec::new();
        let live_msgs = if live_start < messages.len() {
            messages[live_start..].to_vec()
        } else {
            Vec::new()
        };

        return (head_msgs, middle_msgs, live_msgs);
    }

    // Normal case: head + middle + live all non-empty.
    let head_end = boundaries[head_turns];
    let live_start_idx = total_turns - live_turns;
    let live_start = boundaries[live_start_idx];

    let head_msgs = messages[..head_end].to_vec();
    let middle_msgs = messages[head_end..live_start].to_vec();
    let live_msgs = messages[live_start..].to_vec();

    (head_msgs, middle_msgs, live_msgs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a message with a given role and content.
    fn msg(role: &str, content: &str) -> Value {
        serde_json::json!({ "role": role, "content": content })
    }

    /// Build a segment with the given kind, raw messages, and optional summary.
    fn segment(
        kind: SegmentKind,
        raw: Vec<Value>,
        summary: Option<Value>,
        seg_idx: i32,
    ) -> Segment {
        Segment {
            flow_id: "test".to_string(),
            segment_idx: seg_idx,
            kind,
            raw_messages: raw,
            summary_message: summary,
            msg_start_idx: 0,
            msg_end_idx: 0,
            est_tokens: 0,
            raw_est_tokens: 0,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn test_forwarded_messages_head_compressed_live() {
        let head_raw = vec![msg("user", "hello"), msg("assistant", "hi")];
        let comp_raw = vec![msg("user", "old"), msg("assistant", "old reply")];
        let comp_summary = msg("assistant", "summary of old turns");
        let live_raw = vec![msg("user", "new question")];

        let head = segment(SegmentKind::Head, head_raw, None, 0);
        let compressed = segment(SegmentKind::Compressed, comp_raw, Some(comp_summary.clone()), 1);
        let live = segment(SegmentKind::Live, live_raw, None, 2);

        let transcript = Transcript {
            flow_id: "test".to_string(),
            segments: vec![head, compressed, live],
        };

        let forwarded = transcript.forwarded_messages();

        // Head msgs + 1 summary + live msgs
        assert_eq!(forwarded.len(), 2 + 1 + 1);

        // Head messages
        assert_eq!(
            forwarded[0],
            serde_json::json!({ "role": "user", "content": "hello" })
        );
        assert_eq!(
            forwarded[1],
            serde_json::json!({ "role": "assistant", "content": "hi" })
        );

        // Compressed summary (replaces raw for forwarding)
        assert_eq!(
            forwarded[2],
            serde_json::json!({ "role": "assistant", "content": "summary of old turns" })
        );

        // Live messages
        assert_eq!(
            forwarded[3],
            serde_json::json!({ "role": "user", "content": "new question" })
        );
    }

    #[test]
    fn test_turn_boundaries() {
        // system + 3 user/assistant pairs:
        // [0:system, 1:user1, 2:assistant1, 3:user2, 4:assistant2, 5:user3, 6:assistant3]
        // User messages at indices 1, 3, 5.
        // Boundaries: [0, 3, 5] → 3 turns (turn 0 starts at 0, turn 1 starts at 3, turn 2 starts at 5).
        let messages: Vec<Value> = vec![
            msg("system", "you are helpful"),
            msg("user", "question 1"),
            msg("assistant", "answer 1"),
            msg("user", "question 2"),
            msg("assistant", "answer 2"),
            msg("user", "question 3"),
            msg("assistant", "answer 3"),
        ];

        let boundaries = find_turn_boundaries(&messages);
        assert_eq!(boundaries, vec![0usize, 3usize, 5usize]);
    }

    #[test]
    fn test_split_at_turns() {
        // 10-turn conversation (no system), 1 turn = user + assistant pair.
        let mut messages: Vec<Value> = Vec::new();
        for i in 1..=10 {
            messages.push(msg("user", &format!("question {}", i)));
            messages.push(msg("assistant", &format!("answer {}", i)));
        }
        // 20 messages, 10 user messages at indices 0,2,4,6,8,10,12,14,16,18
        // Boundaries: [0, 2, 4, 6, 8, 10, 12, 14, 16, 18]

        let (head, middle, live) = split_messages_at_turns(&messages, 3, 6);

        // head: turns 0..3 (messages 0..6, i.e. 3 turns)
        assert_eq!(head.len(), 6, "head should have 3 turns (6 messages)");

        // middle: turn 3..4 (messages 6..8, i.e. 1 turn)
        assert_eq!(middle.len(), 2, "middle should have 1 turn (2 messages)");

        // live: turns 4..10 (messages 8..20, i.e. 6 turns)
        assert_eq!(live.len(), 12, "live should have 6 turns (12 messages)");

        // Total check
        assert_eq!(head.len() + middle.len() + live.len(), messages.len());
    }

    #[test]
    fn test_split_no_middle() {
        // 5-turn conversation, head=3, live=6 → total turns (5) <= head + live (9)
        // head takes precedence (3 turns), live shrinks to 2 turns, middle empty
        let mut messages: Vec<Value> = Vec::new();
        for i in 1..=5 {
            messages.push(msg("user", &format!("q{}", i)));
            messages.push(msg("assistant", &format!("a{}", i)));
        }
        // 10 messages, 5 turns. Boundaries: [0, 2, 4, 6, 8]

        let (head, middle, live) = split_messages_at_turns(&messages, 3, 6);

        assert!(middle.is_empty(), "middle should be empty when turns < head + live");
        assert_eq!(head.len(), 6, "head should have 3 turns (6 messages)");
        assert_eq!(live.len(), 4, "live should have 2 turns (4 messages)");
        assert_eq!(head.len() + live.len(), messages.len());
    }

    #[test]
    fn test_immutable_flags() {
        let head = segment(SegmentKind::Head, vec![], None, 0);
        let compressed = segment(SegmentKind::Compressed, vec![], None, 1);
        let live = segment(SegmentKind::Live, vec![], None, 2);

        assert!(head.is_immutable(), "Head should be immutable");
        assert!(
            compressed.is_immutable(),
            "Compressed should be immutable"
        );
        assert!(!live.is_immutable(), "Live should NOT be immutable");
    }

    #[test]
    fn test_compression_savings() {
        let head = Segment {
            flow_id: "test".to_string(),
            segment_idx: 0,
            kind: SegmentKind::Head,
            raw_messages: vec![msg("user", "hello")],
            summary_message: None,
            msg_start_idx: 0,
            msg_end_idx: 1,
            est_tokens: 100,
            raw_est_tokens: 100,
            created_at: Utc::now(),
        };

        let compressed = Segment {
            flow_id: "test".to_string(),
            segment_idx: 1,
            kind: SegmentKind::Compressed,
            raw_messages: vec![],
            summary_message: Some(msg("assistant", "summary")),
            msg_start_idx: 1,
            msg_end_idx: 10,
            est_tokens: 2000,
            raw_est_tokens: 5000,
            created_at: Utc::now(),
        };

        let live = Segment {
            flow_id: "test".to_string(),
            segment_idx: 2,
            kind: SegmentKind::Live,
            raw_messages: vec![msg("user", "new")],
            summary_message: None,
            msg_start_idx: 10,
            msg_end_idx: 11,
            est_tokens: 50,
            raw_est_tokens: 50,
            created_at: Utc::now(),
        };

        let transcript = Transcript {
            flow_id: "test".to_string(),
            segments: vec![head, compressed, live],
        };

        // total_raw = 100 + 5000 + 50 = 5150
        // total_fwd  = 100 + 2000 + 50 = 2150
        // savings    = 5150 - 2150 = 3000
        assert_eq!(transcript.total_raw_est_tokens(), 5150);
        assert_eq!(transcript.total_est_tokens(), 2150);
        assert_eq!(transcript.compression_savings(), 3000);
    }

    #[test]
    fn test_turn_boundaries_empty() {
        let boundaries = find_turn_boundaries(&[]);
        assert!(boundaries.is_empty());
    }

    #[test]
    fn test_turn_boundaries_no_user() {
        // No user messages → single turn spanning everything
        let messages = vec![
            msg("system", "init"),
            msg("assistant", "ready"),
        ];
        let boundaries = find_turn_boundaries(&messages);
        assert_eq!(boundaries, vec![0usize]);
    }

    #[test]
    fn test_turn_boundaries_single_user_no_system() {
        // Single user message with no system → [0]
        let messages = vec![msg("user", "hello"), msg("assistant", "hi")];
        let boundaries = find_turn_boundaries(&messages);
        assert_eq!(boundaries, vec![0usize]);
    }

    #[test]
    fn test_turn_count() {
        // 3 user messages → 3 turns
        let head = Segment {
            flow_id: "test".to_string(),
            segment_idx: 0,
            kind: SegmentKind::Head,
            raw_messages: vec![
                msg("system", "init"),
                msg("user", "q1"),
                msg("assistant", "a1"),
                msg("user", "q2"),
                msg("assistant", "a2"),
                msg("user", "q3"),
            ],
            summary_message: None,
            msg_start_idx: 0,
            msg_end_idx: 7,
            est_tokens: 0,
            raw_est_tokens: 0,
            created_at: Utc::now(),
        };
        let transcript = Transcript {
            flow_id: "test".to_string(),
            segments: vec![head],
        };
        assert_eq!(transcript.turn_count(), 3);

        // Empty transcript → 0 turns
        let empty = Transcript {
            flow_id: "test".to_string(),
            segments: vec![],
        };
        assert_eq!(empty.turn_count(), 0);

        // No user messages but non-empty → 1 turn
        let no_user = Segment {
            flow_id: "test".to_string(),
            segment_idx: 0,
            kind: SegmentKind::Head,
            raw_messages: vec![msg("system", "init")],
            summary_message: None,
            msg_start_idx: 0,
            msg_end_idx: 1,
            est_tokens: 0,
            raw_est_tokens: 0,
            created_at: Utc::now(),
        };
        let single_turn = Transcript {
            flow_id: "test".to_string(),
            segments: vec![no_user],
        };
        assert_eq!(single_turn.turn_count(), 1);
    }
}
