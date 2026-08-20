# Plan 003 — Session Fingerprinting for Flow Identity

## Why

The proxy currently derives a flow ID from `X-LLM-Flow-ID` →
`metadata.flow_id` → ephemeral UUID (`src/flow/identify.rs`). Modern
agentic harnesses — Claude Code, opencode, pi — already stamp **stable
session identifiers on every LLM request** via request headers, but
`resolve()` ignores them entirely. Every request from a multi-turn
agentic session therefore lands in its own `ephemeral-{uuid}` flow:

```
Aug 03 18:42:34 proxy: request_id="8c0c1b4b..." flow_id="ephemeral-0b735a43-..."
Aug 03 18:42:37 proxy: request_id="cfdb3f81..." flow_id="ephemeral-9393b1c6-..."
```

With two concurrent sessions the proxy cannot group turns per session,
so per-flow fair scheduling (DRR), queue depth, credits, and
metrics all operate on single requests instead of conversations.

Research (plan background, from PR/docs of each harness):

- **Claude Code** — official gateway protocol docs
  (`code.claude.com/docs/en/llm-gateway-protocol`) specify
  `x-claude-code-session-id` (stable per session),
  `x-claude-code-agent-id`, `x-claude-code-parent-agent-id`.
- **opencode** — merged PR `anomalyco/opencode#20744` adds
  `x-session-affinity: <sessionID>` and `x-parent-session-id` to every
  non-opencode-provider request; current code
  (`packages/opencode/src/session/llm/request.ts`) also sends
  `X-Session-Id` (PR `#31511`), citing it as the convention used by
  Anthropic-compatible proxy providers.
- **pi** — `docs/models.md` documents `sessionAffinityFormat`
  (`x-session-id`, `session_id`/`x-client-request-id`, `x-session-affinity`
  depending on provider); code sends `x-client-request-id` +
  `x-session-affinity` on OpenAI paths and `session_id` on Responses.
  Their `#3579` shows underscored `session_id` gets dropped by strict
  gateways (Envoy/nginx `REJECT_REQUEST`), which is why dash-form names
  are preferred.
- **De-facto standard** — `X-Session-Id` is converging across the
  ecosystem: vLLM (`#48048`/`#48049`, precedence `X-Session-ID` >
  `X-Correlation-ID`), Ray Serve (`x-session-id` default),
  LLMGateway (priority 1), Dvara, langfuse-proxy, Envoy AI Gateway,
  and the Anthropic-proxy convention opencode cites.

## What

Extend `src/flow/identify.rs` to recognize harness session headers so
that all requests of one agentic session resolve to **one stable flow
ID**, before falling through to the body/metadata and ephemeral
fallbacks.

Resolution precedence (highest first):

| # | Source | Harnesses emitting it |
|---|--------|----------------------|
| 1 | `X-LLM-Flow-ID` header | Proxy's explicit override (unchanged) |
| 2 | `x-claude-code-session-id` | Claude Code |
| 3 | `x-session-id` | opencode, pi, standard convention |
| 4 | `x-session-affinity` | opencode, pi |
| 5 | `x-client-request-id` | pi, Codex (Responses cache-affinity) |
| 6 | `session_id` | pi/Codex OpenAI Responses wire header (underscore form, best-effort) |
| 7 | `metadata.flow_id` body | unchanged |
| 8 | ephemeral UUID | unchanged fallback |

Design decisions:

- Header names are matched **case-insensitively** (`http::HeaderMap`
  already normalizes); empty/whitespace-only values fall through to the
  next source.
- The standard `x-session-id` is deliberately ranked above
  `x-session-affinity` so opencode traffic keys on the canonical name.
- `x-parent-session-id`, `x-claude-code-agent-id`,
  `x-claude-code-parent-agent-id` are **not** flow identity: subagent
  requests are scheduled with their session. They are captured in the
  span/log context as a follow-up for trace attribution (see Future
  work), not in this plan.
- No new config for v1: the header list is hardcoded and documented.
  A future `session_headers` config override is noted as an option.
- No `FlowId` type changes: session values are opaque strings, matching
  the current `FlowId::new`.

## Scope

- `src/flow/identify.rs` — header extraction + precedence
- `src/flow/mod.rs` — no changes expected (verify)
- `tests/flow_identify.rs` — unit/integration coverage of header matrix
- Docs: `docs/plans/001-tinyllb/TRACING.md` (field semantics),
  module doc comment in `identify.rs`, `README`/docs if referenced

## Success criteria

- [ ] A multi-turn opencode session produces one stable `flow_id`
      (from `x-session-affinity` / `x-session-id`), verified against a
      header-inspecting test
- [ ] Claude Code turns resolve via `x-claude-code-session-id`
- [ ] pi requests resolve via `x-client-request-id` / `session_id` /
      `x-session-id` per format
- [ ] `X-LLM-Flow-ID` still wins over all harness headers
- [ ] No-identity requests still get unique ephemeral IDs
- [ ] `cargo clippy --all-targets -- -D warnings`,
      `cargo build --all-targets`, `cargo test --all` pass
- [ ] New unit tests cover the full precedence matrix (including empty
      header fall-through and case-insensitivity)

## Task order

```
01 (header resolution in identify.rs)
 → 02 (unit + integration tests)
 → 03 (docs: module comment, TRACING.md)
```

- 01 → 02 (tests verify behavior)
- 03 → last (docs reflect final behavior)

## Future work (not in this plan)

- Agent/parent attribution: log `x-parent-session-id` /
  `x-claude-code-agent-id` / `x-claude-code-parent-agent-id` as span
  fields so subagent requests can be traced to their parent session.
- Configurable session-header names (env override) for private
  harnesses.
- `X-Correlation-ID` / `Idempotency-Key` as low-priority session
  sources, matching vLLM's precedent.
