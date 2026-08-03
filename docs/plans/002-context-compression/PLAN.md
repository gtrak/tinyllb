# Plan 002 — Context Compression

## Why

The vLLM deployment serves Qwen3.6-27B with a 180K-token context window on
2× RTX 5060 Ti (16 GB each). A single 180K request nearly fills GPU KV cache;
a second concurrent request spills to 20 GiB CPU offload, adding latency.
Long agentic chains grow toward the ceiling and eventually hit
context-length-exceeded errors. Multi-user sessions sharing the GPU compound
the problem — one long conversation monopolizes KV cache.

Context compression in the proxy (`llm-qdisc-proxy`) intercepts prompts before
they reach vLLM, maintaining a per-flow transcript and replacing older
conversation turns with compact summaries. This extends effective context
windows for agentic loops, caps per-flow KV footprint, and prevents hard
failures when clients send too much history.

## What

### Segment-based transcript model

Each flow's conversation is modeled as an ordered list of immutable segments:

```
[Head]  [Compressed₁] [Compressed₂] … [Compressedₙ]  [Live]
```

- **Head** — first `head_keep_turns` turns (system prompt + earliest exchanges),
  kept verbatim. This is the prefix-cache anchor — it never changes, so vLLM
  always hits cache on it.
- **Compressed segments** — each is a `{role: "system", content: "Summary of
  turns X–Y: …"}` produced by a background summarization sidecar. Immutable
  once written. Each costs a single prefill, then stays cached forever.
- **Live** — most recent `live_keep_turns` turns, kept verbatim. Grows
  turn-by-turn. When total context exceeds `compress_threshold`, the oldest
  chunk of Live turns is folded into a new Compressed segment.

When total estimated tokens exceed `compress_threshold`, the proxy enqueues a
background compression job. The worker makes a non-streaming sidecar
`POST /v1/chat/completions` to the same vLLM backend (tagged as
`background`-priority) to summarize the chunk, stores the result, and shrinks
the Live segment. Subsequent requests automatically pick up the new compressed
version.

### Prefix-cache friendliness

The outgoing `messages` array is `[Head + Compressed₁..ₙ + Live]`. The prefix
`[Head + Compressed₁..ₙ]` is byte-identical across requests, so vLLM's prefix
cache always hits on it — only the new Live suffix needs prefill. The one-time
cost of introducing a new Compressed segment is a single prefill of that
segment.

### Stateful per-flow, SQLite-backed

The proxy maintains a conversation transcript per flow ID (from
`X-LLM-Flow-ID` → `metadata.flow_id` → ephemeral UUID). Transcripts are stored
in SQLite (via `sqlx`) and survive proxy restarts. The proxy is the source of
truth for what vLLM sees — the client's older messages are substituted with
the proxy's compressed versions.

### Same-backend sidecar compression

Compression requests go to the same vLLM instance (`localhost:8000`) through
the proxy itself, tagged as `background`-priority so they don't preempt user
traffic. The `X-LLM-Internal: compressor` header marks them for the proxy to
skip compression (preventing infinite recursion).

### Turn model

A **turn** is defined as a user message followed by all non-user messages
until the next user message (or end of array). Concretely: a turn boundary
is at the start of each `role: "user"` message. `head_keep_turns = 3` means
keep the first 3 turns verbatim in the Head segment. `live_keep_turns = 6`
means keep the last 6 turns verbatim in the Live segment.

## Scope

All three phases in one pass:

- **Foundation**: dependencies, config, token estimator, segment model,
  SQLite store, reconciliation
- **Compression core**: context state + AppState integration, body rewriting
  in proxy_handler, summarization prompt, background compression worker
- **Polish**: admin API, Prometheus metrics, integration tests, docs +
  deployment config

## Success criteria

- [ ] Long-running agent flow (>100K tokens of history) stays under
  `compress_threshold` after compression kicks in
- [ ] Compressed segments are immutable; vLLM prefix cache hits on
  `[Head + Compressed]` prefix across requests (verified via vLLM metrics)
- [ ] Sidecar compression requests are tagged `background`-priority and
  don't preempt interactive traffic
- [ ] Transcripts persist across proxy restarts (SQLite)
- [ ] Divergent client history (client resets conversation) is detected and
  handled gracefully (new transcript)
- [ ] All integration tests pass
- [ ] Prometheus metrics expose: compression events, tokens saved, sidecar
  latency, estimated context size per flow
- [ ] Admin API: `GET /admin/context/{flow_id}` returns segment breakdown
- [ ] Proxy fails open: if compression subsystem errors, forward original
  request body unchanged

## Task order

```
01 (deps + config)
 → 02 (token estimator)
 → 03 (segment model)
 → 04 (SQLite store)
 → 05 (reconciliation)
 → 06 (context state + AppState)
 → 07 (body rewriting in proxy_handler)
 → 08 (summarization prompt)
 → 09 (background compression worker)
 → 10 (admin API)
 → 11 (Prometheus metrics)
 → 12 (integration tests)
 → 13 (docs + deployment)
```

Dependency graph:

```
01 ──→ 02 ──→ 05 ──→ 06 ──→ 07 ──→ 12
 │             ↑      │      ↓
 └──→ 03 ──→ 04 ──→  09 ←── 08    13
             ↓        ↓
             10       11
```

- 01 → all (deps must compile)
- 02 → 05, 07, 11 (token counting needed)
- 03 → 04, 05, 09 (segment types needed)
- 04 → 05, 09, 10, 12 (store needed)
- 05 → 06, 07 (reconcile needed for state + rewriting)
- 06 → 07, 09, 10 (context state needed)
- 08 → 09 (prompt needed by worker)
- 09 → 11, 12 (worker needed for metrics + tests)
- 07 → 12 (body rewriting tested in integration)
- 10 → 12 (admin API tested)
- 11 → 12 (metrics tested)
- 13 → last (docs after implementation)
