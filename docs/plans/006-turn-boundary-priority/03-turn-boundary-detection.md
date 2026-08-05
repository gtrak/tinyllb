# 03 — Turn-Boundary Detection

**Parent:** `PLAN.md`

## Objective

Add a function in `src/gateway/proxy.rs` that inspects the request body and
determines whether the current request is a turn boundary (`role: "user"` /
`"system"` / non-chat) or an intra-turn continuation (`role: "tool"` /
`"assistant"`). This signal feeds into `admit_with_turn_boundary()` (task 04).

This is independent of tasks 01 and 02 — it only touches the proxy handler
and a new helper function.

## Files

| File | Change |
|---|---|
| `src/gateway/proxy.rs` | Add `is_turn_boundary_request()`; call `admit_with_turn_boundary()` at line ~636 |

## Steps

### 1. Add the helper function

Place near the other body-parsing helpers
(`body_wants_streaming`, `extract_max_tokens`, `inject_include_usage` —
around line 84-143):

```rust
/// Determine whether this request represents a turn boundary (the user
/// is initiating a new message) vs an intra-turn continuation (the agent
/// is sending a tool result or prefill).
///
/// Rule:
/// - `messages[last].role == "user"` or `"system"` → turn boundary
/// - `messages[last].role == "tool"` or `"assistant"` → NOT a turn boundary
/// - Non-JSON body, no `messages` array, or empty array → turn boundary
///   (optimistic — consistent with the cold-start philosophy)
///
/// This signal tells the cadence state machine whether the *previous* gap
/// was human think time (idle, at a turn boundary) or tool execution time
/// (intra-turn). See `docs/plans/006-turn-boundary-priority/PLAN.md`.
fn is_turn_boundary_request(body: &bytes::Bytes) -> bool {
    if body.is_empty() {
        return true;
    }
    let value: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return true,
    };
    let messages = match value.get("messages").and_then(|m| m.as_array()) {
        Some(m) if !m.is_empty() => m,
        _ => return true,
    };
    match messages.last() {
        Some(msg) => {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            // "user" and "system" → the human is initiating.
            // "tool" → agent sending a tool result (intra-turn).
            // "assistant" → prefill continuation (intra-turn).
            // Unknown role → optimistic (treat as turn boundary).
            role == "user" || role == "system" || (role != "tool" && role != "assistant")
        }
        None => true,
    }
}
```

Note the boolean logic: the final `||` clause catches unknown roles
optimistically — anything that is not explicitly `"tool"` or `"assistant"`
is treated as a turn boundary.

### 2. Wire it into the proxy handler

At `proxy.rs:636`, replace the current admit call:

```rust
// OLD:
let _ticket = match state.scheduler.admit(flow_id_for_admit, work_unit).await {

// NEW:
let is_turn_boundary = is_turn_boundary_request(&body_bytes);
let _ticket = match state.scheduler
    .admit_with_turn_boundary(flow_id_for_admit, work_unit, is_turn_boundary)
    .await
```

`body_bytes` is the request body after context compression rewrite (line
~581-583). It may have been replaced by the compression step. The last
message's role is still in the (possibly rewritten) body — context
compression preserves the live tail, so the last message role is intact.

### 3. Add tracing span field (optional)

Record the turn-boundary flag in the admit span for observability:

```rust
// In the admit span record block (proxy.rs:570-571):
span.record("is_turn_boundary", is_turn_boundary);
```

Or record it in the `admit_with_turn_boundary` method on the scheduler
side (task 04).

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
```

The build will fail at this point because `admit_with_turn_boundary` doesn't
exist yet (task 04 adds it). To verify `is_turn_boundary_request` in
isolation, add an inline `#[cfg(test)]` module with test cases:

| Body | Expected |
|---|---|
| `{"messages":[{"role":"user","content":"hi"}]}` | `true` |
| `{"messages":[{"role":"user","content":"hi"},{"role":"assistant","content":"hi"},{"role":"tool","content":"result"}]}` | `false` |
| `{"messages":[{"role":"assistant","content":"prefill"}]}` | `false` |
| `{"messages":[{"role":"system","content":"you are..."}`]}` | `true` |
| `{"messages":[]}` | `true` |
| `{"prompt":"hello"}` (non-chat) | `true` |
| `""` (empty body) | `true` |
| `not json` | `true` |
| `{"messages":[{"role":"unknown","content":"x"}]}` | `true` (optimistic) |

Run with `cargo test --bin tinyllb is_turn_boundary` (or as a lib test
depending on where the function lives — it can be a free function tested via
`#[cfg(test)] mod tests` inside `proxy.rs`).
