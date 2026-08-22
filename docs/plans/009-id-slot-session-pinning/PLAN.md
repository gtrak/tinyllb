# Plan 009 — id_slot Session→Slot Pinning

Pin each tracked session (a `FlowId`) to a stable llama.cpp backend slot via the
`id_slot` request parameter, so a multi-turn conversation's KV cache stays warm
in one slot across turns (prompt-cache hit → lower time-to-first-token).

## Problem

llama-server (`--parallel N`) runs N slots, each with its own KV cache. Today
the proxy forwards every request with `id_slot = -1` (auto-select), so the
server picks an arbitrary free slot per request. A conversation's follow-up
turns may land on a *different* slot than the previous turn, so the prompt's KV
cache does not carry over — every turn re-encodes the full prompt. llama.cpp's
HTTP API accepts an integer `id_slot` field that pins a request to a specific
slot (verified: `server-task.h` `int id_slot = -1`; `get_slot_by_id` at
`server-context.cpp:1473` does `id_slot % slots.size()` — out-of-range wraps,
slots are 0-indexed 0..N-1, and the response echoes the `id_slot` actually used).

The proxy already tracks the unit that corresponds to a "session": a
`FlowId` (`src/flow/mod.rs:48`), resolved per request from harness session
headers / `metadata.flow_id`, or an ephemeral auto-UUID (`identify::resolve`,
`src/flow/identify.rs:31`). It is in scope exactly where the outgoing backend
body is built (`proxy_handler`, `src/gateway/proxy.rs:461` → `:485-497`). The
body is a `serde_json::Value`/`Bytes` pass-through with an existing
field-injection precedent (`inject_include_usage`, `proxy.rs:108`).

## Goals

- For a **named** (non-ephemeral) session, inject `id_slot` into the outgoing
  request so all its turns pin to the same llama.cpp slot → KV cache reuse.
- **Stateless & deterministic**: slot = `fnv1a(flow_id) % N`. Stable across
  restarts, no per-flow bookkeeping, no coupling to flow reaping.
- **Opt-in & backend-safe**: disabled by default; a vLLM deployment is
  byte-identical. llama.cpp-specific by config name/purpose.
- **No behavior change** to scheduling, admission, KV gate, retries, or token
  accounting.

## Non-goals

- Per-slot / per-session **KV attribution** (reading which slot a session is
  on and its KV usage back from `/slots`). That's a follow-up on top of this;
  this plan only *assigns* slots, it does not *observe* them.
- Free-list slot allocation (first-free-slot-per-flow) with lifecycle cleanup.
  A deterministic hash is the correct starting point (see Rationale).
- vLLM slot pinning (vLLM has no slot concept; the feature is gated off).
- Changing the proxy-side concurrency cap (`max_active_flows`) — unrelated.

## Design

### Config — `backend.llamacpp_slots: Option<u32>`

- `None` (default) → pinning disabled; no `id_slot` injected. vLLM-safe.
- `Some(n)` with `n >= 1` → pin named flows to `fnv1a(flow_id) % n`.
- Validation: if `Some(0)` → config error (cannot pin into 0 slots).
- The operator sets this to mirror llama-server's `--parallel N`. (If set
  higher than the real count, llama.cpp wraps `id_slot % slots.size()`, so it
  never breaks — it just load-biases; if lower, it under-uses slots.)

Chosen over auto-detecting N from `/slots` (rejected alternatives):
- Auto-detect needs the `--slots` flag on the server, has a cold-start gap
  (N unknown before the first scrape), and adds runtime state to the monitor.
  A static config value is predictable, self-gating, and matches what the
  operator already knows (`--parallel`).

### Hash — `slot_id_for_flow(flow: &str, slot_count: u32) -> u32`

- FNV-1a 64-bit over the flow-id bytes, `mod slot_count` → `[0, slot_count)`.
- **Must be deterministic** — Rust's default `HashMap` hasher is randomized
  per-process and would re-shuffle pinning on every restart (defeating cache
  reuse). FNV-1a has no dependency and is stable.
- `slot_count == 0` → `0` (defensive; validation forbids it).
- Lives in `src/flow/mod.rs` (a pure function of flow identity; unit-tested).

### Injection — `proxy_handler`

At the outgoing-body build site (`proxy.rs:485-497`), after `include_usage`:

- Compute `is_inference` (hoist the existing `is_inference_request` call).
- For an **inference** request where the flow is **not ephemeral** and
  `llamacpp_slots` is `Some(n)`: `slot = slot_id_for_flow(&flow_id.to_string(), n)`;
  inject `id_slot = slot` (a JSON **integer**) into the body.
- Ephemeral flows and non-inference requests: no `id_slot` (auto-select).
- **Compose with `include_usage`**: produce a single final `forwarded_body`;
  drop the forwarded `Content-Length` iff the body bytes changed from the
  original. Baking `id_slot` into `forwarded_body` means transient-retry and
  premature-stop re-forwards (which re-send `forwarded_body`) carry it with no
  extra work.

Correctness anchors:
- `id_slot` is an integer in the wire JSON (llama.cpp `int id_slot`).
- Only `POST /v1/chat/completions` and `POST /v1/completions` get it;
  `GET /v1/models` never does (empty body → injection is a no-op anyway, but
  gated on `is_inference` for clarity).

## Task breakdown

| # | Task | Files | Complexity |
|---|------|-------|-----------|
| 01 | Config `llamacpp_slots` + `slot_id_for_flow` hash + unit tests | config/mod.rs, config/loader.rs, flow/mod.rs, config.example.yaml, tests/config.rs | S |
| 02 | AppState field + `id_slot` injection in `proxy_handler` + integration tests | gateway/mod.rs, gateway/proxy.rs, main.rs, tests/ (stub-backend) | M |
| 03 | Docs: lat.md section + cross-refs, README, config example, archive issue 04 | lat.md/gateway.md (+ flow/config cross-links), README.md, issues/04-*.md | S |

Each task: delegate to `worker` → review to `reviewer` (ACCEPT/FIX/…) → commit.

## Verification (every task)

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all
lat check
```

Global regression gate: disabled-by-default ⇒ `llamacpp_slots: None` ⇒ **zero**
`id_slot` in any outbound body ⇒ all existing tests pass unchanged.

## Rollout (operator)

For a llama.cpp backend with `--parallel 4`:
```yaml
backend:
  llamacpp_slots: 4   # mirror --parallel; enables id_slot pinning
```
Named sessions (via `x-session-id` / `x-claude-code-session-id` /
`metadata.flow_id`) now pin to a stable slot. Ephemeral requests keep
auto-slot-selection. vLLM deployments leave `llamacpp_slots` unset.
