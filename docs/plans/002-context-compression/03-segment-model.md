# Issue 03 — Segment Model + Transcript Types

## Objective

Define the core data structures for the segment-based transcript model. These
types are used by the store (issue 04), reconciliation (issue 05), and
compression worker (issue 09).

## Files

| File | Change |
|------|--------|
| `src/context/segment.rs` | New — `SegmentKind`, `Segment`, `Transcript` |
| `src/context/mod.rs` | Add `pub mod segment;` |

## Prerequisites

- Issue 02 (token estimator — for `estimate_messages`)

## Steps

1. **`SegmentKind` enum**:
   ```rust
   #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
   #[serde(rename_all = "lowercase")]
   pub enum SegmentKind {
       Head,       // first N turns, kept verbatim
       Compressed, // summary message replacing original turns
       Live,       // most recent turns, kept verbatim
   }
   ```

2. **`Segment` struct**:
   ```rust
   #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
   pub struct Segment {
       pub flow_id: String,
       pub segment_idx: i32,
       pub kind: SegmentKind,
       /// Original raw messages for this segment's turn range.
       /// For Head/Live: the actual messages forwarded to vLLM.
       /// For Compressed: the original messages that were compressed away
       ///   (kept for audit / potential re-compression).
       pub raw_messages: Vec<serde_json::Value>,
       /// For Compressed only: the summary message sent to vLLM.
       /// Head/Live forward `raw_messages` directly.
       pub summary_message: Option<serde_json::Value>,
       /// Start message index (inclusive) in the original conversation.
       pub msg_start_idx: i32,
       /// End message index (exclusive) in the original conversation.
       pub msg_end_idx: i32,
       /// Estimated tokens of what's forwarded (raw for head/live, summary
       /// for compressed).
       pub est_tokens: i32,
       /// Estimated tokens of the original raw messages (for compressed:
       /// what the raw would've cost; for head/live: same as est_tokens).
       pub raw_est_tokens: i32,
       pub created_at: chrono::DateTime<chrono::Utc>,
   }
   ```

3. **`Segment::forwarded_messages(&self) -> Vec<&serde_json::Value>`**:
   - `Head` / `Live` → `self.raw_messages.iter().collect()`
   - `Compressed` → `self.summary_message.iter().collect()` (single message)

4. **`Segment::is_immutable(&self) -> bool`**:
   - `Head` → true (never modified after creation)
   - `Compressed` → true (never modified after creation)
   - `Live` → false (grows as new turns arrive)

5. **`Transcript` struct**:
   ```rust
   #[derive(Debug, Clone)]
   pub struct Transcript {
       pub flow_id: String,
       pub segments: Vec<Segment>,
   }
   ```

6. **`Transcript` methods**:
   - `forwarded_messages(&self) -> Vec<serde_json::Value>` — concatenate
     `forwarded_messages()` from all segments in order. Returns the full
     `messages` array to send to vLLM.
   - `total_est_tokens(&self) -> usize` — sum of `est_tokens` across all
     segments.
   - `total_raw_est_tokens(&self) -> usize` — sum of `raw_est_tokens` (the
     uncompressed cost).
   - `compression_savings(&self) -> usize` — `total_raw - total_forwarded`.
   - `head_segment(&self) -> Option<&Segment>` — first Head segment.
   - `live_segment(&self) -> Option<&Segment>` — the Live segment (there
     should be exactly one; the last segment by convention).
   - `compressed_segments(&self) -> impl Iterator<Item = &Segment>` — filter
     kind == Compressed.
   - `turn_count(&self) -> usize` — count turns across all segments
     (a turn = user message + all following non-user messages).
   - `split_at_turn_boundary(messages: &[Value], turn_idx: usize) -> (usize, usize)`
     — given a slice of messages and a turn index, return the
     `(msg_start_idx, msg_end_idx)` for that turn. A turn boundary is the
     start of a `role: "user"` message.

7. **Turn boundary detection** — helper function
   `find_turn_boundaries(messages: &[Value]) -> Vec<usize>`:
   - Returns message indices where each turn starts
   (i.e., indices where `role == "user"`).
   - Turn 0 = messages[0..first_user_end], turn 1 = next user start, etc.
   - If conversation starts with a system message (common), turn 0 includes
     the system message + the first user message + assistant response + any
     tool calls until the next user message.

8. **`split_messages_at_turns(messages, head_turns, live_turns) -> (head_msgs, middle_msgs, live_msgs)`**:
   - Given the full messages array, split into three slices:
     - `head_msgs`: messages belonging to the first `head_turns` turns
     - `live_msgs`: messages belonging to the last `live_turns` turns
     - `middle_msgs`: everything in between (candidates for compression)
   - If total turns <= head_turns + live_turns, `middle_msgs` is empty
     and head/live overlap is resolved (head takes precedence, live shrinks).

9. **Unit tests**:
   - `test_forwarded_messages_head_compressed_live` — 1 head + 1 compressed
     + 1 live segment → forwarded = [head msgs, summary msg, live msgs]
   - `test_turn_boundaries` — messages array with system + 3 user/assistant
     pairs → 4 turn boundaries (system+user1 is turn 0, etc.)
   - `test_split_at_turns` — 10-turn conversation, head=3, live=6 →
     head has 3 turns, live has 6 turns, middle has 1 turn
   - `test_split_no_middle` — 5-turn conversation, head=3, live=6 →
     middle is empty, live shrinks to 2
   - `test_immutable_flags` — head and compressed are immutable, live is not
   - `test_compression_savings` — compressed segment with raw_est_tokens=5000,
     est_tokens=2000 → savings = 3000

## Verification

```bash
cargo test --lib segment 2>&1 | tail -10
```
