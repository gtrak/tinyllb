# Issue 02 — Session Header Test Matrix

## Objective

Cover the new precedence in `src/flow/identify.rs` with unit tests
(in-file) and an HTTP-level integration test (in `tests/flow_identify.rs`),
so the harness fingerprinting is locked in.

## Files

| File | Change |
|------|--------|
| `src/flow/identify.rs` | Unit tests in the existing `mod tests` block |
| `tests/flow_identify.rs` | New/extended integration tests exercising `resolve()` through the proxy |

## Steps

1. **Unit tests in `src/flow/identify.rs` `mod tests`:**

   - `claude_code_session_id_is_extracted` — header
     `x-claude-code-session-id: ses_abc` → `FlowId("ses_abc")`,
     not ephemeral.
   - `standard_x_session_id_is_extracted` — `x-session-id: ses_xyz` →
     `ses_xyz`.
   - `x_session_affinity_is_extracted` — `x-session-affinity: ses_42` →
     `ses_42`.
   - `x_client_request_id_is_extracted` — `x-client-request-id: abc-uuid`
     → `abc-uuid`.
   - `underscore_session_id_is_extracted` — `session_id: codex-s-1` →
     `codex-s-1`.
   - `x_llm_flow_id_overrides_harness_headers` — set BOTH
     `x-llm-flow-id: my-flow` and `x-claude-code-session-id: ses_other`
     → `my-flow`.
   - `harness_headers_are_case_insensitive` — `X-Session-Id: Ses_1`
     (capitalized) → `Ses_1` (HeaderMap normalizes case).
   - `empty_harness_headers_fall_through_to_body` — empty
     `x-session-id: ""` plus `metadata.flow_id: "agent-2"` in body →
     `agent-2`.
   - `empty_harness_headers_fall_through_to_ephemeral` — empty
     `x-session-id` and no body metadata → ephemeral ID.
   - `session_headers_beat_metadata_flow_id` — `x-session-id: ses_a`
     plus body `metadata.flow_id: agent-b` → `ses_a`.
   - `claude_code_has_priority_over_x_session_id` — both present →
     the `x-claude-code-session-id` value wins.

2. **Integration test in `tests/flow_identify.rs`** (mirror existing
   `test_header_flow_id_resolved` style):
   - Send `POST /v1/chat/completions` with `X-Session-Id: integ-session`
     and assert the resolved flow ID is `integ-session` (non-ephemeral).
   - Send with `x-session-affinity: integ-affinity` and assert
     `integ-affinity`.
   - Send two requests with the same `X-Session-Id` and assert both
     resolve to the same flow / queue position (regression for the
     original "same IDs per agentic session" bug).

3. **Tracing note (no code change in this issue)**: the `flow_id`
   span field already inherits the resolved ID, so a passing test
   implies the trace shows the session ID.

## Verification

```bash
cargo build --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --lib flow 2>&1 | tail -10
cargo test --test flow_identify 2>&1 | tail -15
cargo test --all 2>&1 | tail -10
```