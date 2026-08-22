# id_slot Session→Slot Pinning

## Problem

llama-server (`--parallel N`) runs N slots, each with its own KV cache. The
proxy forwards every request with `id_slot = -1` (auto-select), so the server
picks an arbitrary free slot per request. A conversation's follow-up turns may
land on a *different* slot than the previous turn, so the prompt's KV cache does
not carry over — every turn re-encodes the full prompt history, inflating
time-to-first-token for multi-turn sessions.

llama.cpp's HTTP API accepts an integer `id_slot` field that pins a request to a
specific slot (verified against source: `server-task.h` `int id_slot = -1`;
`get_slot_by_id` at `server-context.cpp:1473` does `id_slot % slots.size()` —
out-of-range wraps, slots are 0-indexed 0..N-1; the response echoes the
`id_slot` actually used).

The proxy already tracks the unit that corresponds to a "session": a `FlowId`
(`src/flow/mod.rs:48`), resolved per request from harness session headers
(`x-session-id`, `x-claude-code-session-id`, …) or `metadata.flow_id`, else an
ephemeral auto-UUID. It is in scope exactly where the outgoing backend body is
built (`proxy_handler`, `src/gateway/proxy.rs:461` → `:485-497`), and the body
is a `serde_json::Value` pass-through with an existing field-injection precedent
(`inject_include_usage`, `proxy.rs:108`).

## Current behavior

```
every request: id_slot = -1  (llama.cpp auto-selects a free slot)
consequences:
  - a session's turns scatter across slots
  - no prompt KV-cache reuse across turns of the same conversation
  - higher TTFT on follow-up turns
```

## Desired behavior

```
named session (stable FlowId):
  id_slot = fnv1a(flow_id) % N      # N = backend.llamacpp_slots
  - same session always pins to the same slot across turns and restarts
  - prompt KV cache reuses → lower TTFT on follow-up turns

ephemeral (one-shot) request:
  id_slot omitted (-1)  → auto-select a free slot (lowest latency)

vLLM / llamacpp_slots unset:
  id_slot never injected  → byte-identical to today
```

## Constraints

- Must be deterministic (stable across proxy restarts) — a randomized hasher
  would re-shuffle pinning and defeat cache reuse.
- Must not require per-flow slot bookkeeping or lifecycle cleanup.
- Must be opt-in and safe for vLLM (no slot concept).
- Must not change scheduling, admission, KV gate, retry, or token accounting.
- `id_slot` must be a JSON **integer** on the wire.

## Out of scope

- Per-slot/per-session KV *attribution* (observing which slot a session is on
  and its KV usage from `/slots`) — a follow-up on top of this.
- Free-list slot allocation with lifecycle cleanup.
- vLLM pinning.
