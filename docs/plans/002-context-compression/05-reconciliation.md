# Issue 05 — Reconciliation

## Objective

Implement the logic that takes an incoming request's `messages` array and
reconciles it against the stored transcript for that flow. This detects new
turns, handles divergent history (client reset), and updates the Live segment.

The reconciliation function is the "read + update" path — it does not perform
compression itself, but it determines whether compression is needed and
returns the forwarded messages for the current request.

## Files

| File | Change |
|------|--------|
| `src/context/reconcile.rs` | New — `reconcile()` function + `ReconcileResult` |
| `src/context/mod.rs` | Add `pub mod reconcile;` |

## Prerequisites

- Issue 02 (token estimator)
- Issue 03 (segment model — `Transcript`, `Segment`, turn boundary functions)
- Issue 04 (store — `TranscriptStore` for reading/writing)

## Steps

1. **`ReconcileResult` struct**:
   ```rust
   pub struct ReconcileResult {
       /// Messages to forward to vLLM (head raw + compressed summaries + live raw)
       pub forwarded_messages: Vec<serde_json::Value>,
       /// Total estimated tokens of forwarded_messages
       pub total_est_tokens: usize,
       /// Total estimated tokens of the uncompressed conversation
       pub total_raw_est_tokens: usize,
       /// True if total_est_tokens exceeds compress_threshold
       pub needs_compression: bool,
       /// Range of turn indices that should be compressed next (if needs_compression)
       pub compress_turn_range: Option<(usize, usize)>,
       /// Whether the transcript was reset (divergence detected)
       pub transcript_reset: bool,
   }
   ```

2. **`reconcile()` function signature**:
   ```rust
   pub async fn reconcile(
       flow_id: &str,
       incoming_messages: &[serde_json::Value],
       store: &dyn TranscriptStore,
       estimator: &TokenEstimator,
       config: &ContextPolicy,
   ) -> anyhow::Result<ReconcileResult>
   ```

3. **Core reconciliation algorithm**:

   a. **Load existing transcript** from store. If none exists, create a new
      one:
      - Split `incoming_messages` at `head_keep_turns` / `live_keep_turns`
      - Create Head segment (first `head_keep_turns` turns)
      - Create Live segment (remaining turns, or last `live_keep_turns` turns
        if total turns > head + live)
      - If total turns <= head + live: Head = first head_turns, Live = rest
      - Save to store
      - Return forwarded_messages = incoming_messages as-is

   b. **If transcript exists**, diff incoming against stored Live:
      - Extract stored Live segment's `raw_messages`
      - Find the longest common suffix between incoming and stored Live:
        - `incoming_tail` = last N messages of incoming
        - `stored_live` = stored Live messages
        - Find the largest k such that
          `incoming[incoming.len()-k..]` matches `stored_live` from some
          offset (i.e., the incoming's tail extends or matches stored live)
      - **New turns** = messages in incoming beyond what's in stored Live
      - Append new turns to the Live segment

   c. **Divergence detection**:
      - If the incoming messages' head (first M messages) does not match the
        stored Head segment's messages at all (common prefix length < 1 turn):
        - The client has reset the conversation (different system prompt or
          new session)
        - **Reset**: archive old transcript (rename or delete), create a fresh
          transcript from incoming_messages
        - Set `transcript_reset = true`
      - If incoming's head partially matches but then diverges in the middle:
        - Log warning, keep the stored transcript, only accept new turns from
          the tail (conservative — don't corrupt on partial mismatches)

   d. **Compute token counts** on the reconciled transcript:
      - `forwarded_messages` = `transcript.forwarded_messages()`
      - `total_est_tokens` = `estimator.estimate_messages(&forwarded_messages)`
      - `total_raw_est_tokens` = sum of `raw_est_tokens` across segments

   e. **Check compression trigger**:
      - If `total_est_tokens > config.compress_threshold`:
        - `needs_compression = true`
        - Determine `compress_turn_range`: the oldest `compress_chunk_turns`
          turns in the Live segment that are NOT within `head_keep_turns`
          (i.e., turns between Head and the most recent `live_keep_turns`)
        - If no turns are available to compress (Live is too short),
          `compress_turn_range = None` (edge case: head + live all within
          threshold but somehow over — shouldn't happen if config is sane)

   f. **Save updated Live segment** to store
   g. **Update transcript meta** with new token counts
   h. Return `ReconcileResult`

4. **Edge cases**:
   - **Empty messages array**: return empty forwarded_messages, tokens=0
   - **Single message**: treat as Head only, no Live, no compression
   - **Tool call messages**: messages with `role: "tool"` or `tool_calls`
     field belong to the turn of the preceding user message
   - **No new turns** (incoming matches stored exactly): return stored
     forwarded_messages, no store update needed
   - **Incoming is shorter than stored Live**: client truncated history —
     use incoming as-is (new transcript or trust the client's truncation)

5. **Longest common suffix matching** — helper:
   ```rust
   fn find_new_turns(
       incoming: &[Value],
       stored_live: &[Value],
   ) -> Vec<Value>
   ```
   - Compare from the end of both arrays
   - Find where the incoming tail starts to diverge from stored_live
   - Everything after the divergence point in incoming is "new"
   - If stored_live is empty: all of incoming beyond head is new
   - If incoming is shorter than stored_live: return empty vec (nothing new)

   More precisely:
   - Find the largest `match_len` such that the last `match_len` messages of
     `stored_live` equal the last `match_len` messages of `incoming` (if
     `incoming.len() >= stored_live.len()`)
   - New turns = `incoming[stored_live.len() - match_len..]` (i.e., the
     messages beyond what was already stored)
   - If `match_len == 0` and `incoming != stored_live`: potential divergence,
     check if it's a conversation reset

6. **Unit tests**:
   - `test_new_transcript` — first request for a flow, creates Head + Live
   - `test_append_new_turns` — existing transcript, incoming has 2 more
     messages → Live extended
   - `test_no_new_turns` — incoming matches stored exactly → no update
   - `test_conversation_reset` — incoming has different system prompt →
     transcript_reset = true
   - `test_triggers_compression` — total tokens > threshold →
     needs_compression = true, compress_turn_range set correctly
   - `test_no_compression_under_threshold` — tokens < threshold →
     needs_compression = false
   - `test_tool_calls_in_turns` — messages with tool calls correctly
     grouped into turns
   - `test_empty_messages` — empty array returns empty result
   - `test_incoming_shorter_than_stored` — client truncated history
   - `test_head_boundary` — head_keep_turns=2, conversation with 5 turns →
     head has 2 turns, live has 3

## Verification

```bash
cargo test --lib reconcile 2>&1 | tail -10
```
