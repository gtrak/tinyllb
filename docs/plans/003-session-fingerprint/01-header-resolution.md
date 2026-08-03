# Issue 01 — Session Header Resolution in `identify.rs`

## Objective

Extend `resolve()` in `src/flow/identify.rs` so requests from agentic
harnesses (Claude Code, opencode, pi) resolve to the harness's stable
session ID instead of a per-request ephemeral UUID. Keep
`X-LLM-Flow-ID` as the highest-precedence explicit override and
`metadata.flow_id` / ephemeral as the fallbacks.

## Files

| File | Change |
|------|--------|
| `src/flow/identify.rs` | Add header extraction helpers + session-header precedence in `resolve()`; update module doc comment |

No changes expected to `src/flow/mod.rs`, `src/gateway/proxy.rs`, or
the scheduler — `resolve` is already called with `&HeaderMap` and
`&Bytes` at the gateway layer.

## Steps

1. **Keep existing precedence 1 and 2 unchanged**: `X-LLM-Flow-ID`
   header first, then body `metadata.flow_id`.

2. **Insert session-header extraction between them** as a helper,
   e.g.:

   ```rust
   /// Extract a stable session ID from harness session headers.
   ///
   /// Order (highest to lowest precedence):
   /// 1. x-claude-code-session-id   (Claude Code)
   /// 2. x-session-id               (de-facto standard: opencode, pi, vLLM,
   ///                                Anthropic-compatible proxy convention)
   /// 3. x-session-affinity         (opencode, pi)
   /// 4. x-client-request-id        (pi / Codex OpenAI-compatible paths)
   /// 5. session_id                 (pi / Codex Responses wire header;
   ///                                underscore form, best-effort)
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
   ```

3. **Wire into `resolve()`** between the `X-LLM-Flow-ID` check and the
   body check:

   ```rust
   // 2. Try harness session headers (Claude Code, opencode, pi, standard).
   if let Some(session_id) = extract_flow_id_from_session_headers(headers) {
       return session_id;
   }
   ```

   Renumber the existing body/metadata and ephemeral steps in the
   doc comment to reflect the new order.

4. **Update the module doc comment** in `identify.rs` to document the
   full precedence list (see PLAN.md table) and note that header names
   are matched case-insensitively.

5. **Verify no behavior change** for existing paths: `X-LLM-Flow-ID`
   still wins; no-header requests still get ephemeral IDs.

## Verification

```bash
cargo build --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --lib flow 2>&1 | tail -10
```
