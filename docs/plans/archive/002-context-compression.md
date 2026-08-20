> Status: Archived 2026-08-04 as implemented. The context-compression subsystem was subsequently deleted on 2026-08-19 (commit d400a93). This document is retained as a historical design record; the feature no longer exists in the codebase.

# 002 — Context Compression

The vLLM deployment (Qwen3.6-27B, 180K context, 2× RTX 5060 Ti) was hitting
KV-cache exhaustion and context-length-exceeded errors on long agentic chains.
Context compression was added to the `tinyllb` to intercept prompts
before they reach vLLM, maintaining a per-flow transcript and replacing older
turns with compact summaries. This extends effective context windows, caps
per-flow KV footprint, and prevents hard failures from oversized history.

Each flow's conversation is modeled as `[Head + Compressed₁..ₙ + Live]`. Head
(system prompt + earliest turns) and Compressed segments are immutable and
prefix-cache-friendly; Live grows per request. When estimated tokens exceed
`compress_threshold`, the oldest chunk of Live is summarized via a background
sidecar `POST /v1/chat/completions` to vLLM and stored as a new Compressed
segment. Transcripts persist in SQLite (`sqlx`), survive proxy restarts, and
the proxy fails open (forwards original body) on any compression-subsystem
error. Disabled by default; enabled via `context_policy.enabled`.

## Scope
- **Foundation**: `sqlx`/`tokenizers` deps, `ContextPolicy` config, token
  estimator (HF tokenizer + heuristic fallback), segment model + turn
  boundaries, SQLite store (WAL), reconciliation (read+update path)
- **Compression core**: `ContextState` + per-flow locks + mpsc channel, body
  rewriting in `proxy_handler`, summarization prompt builder, background
  compression worker (sidecar + retry + backoff)
- **Polish**: admin API (`/admin/context/*`), Prometheus metrics
  (`tinyllb_context_*`), integration tests, docs + deployment config

## Tasks
1. deps + config — 2. token estimator — 3. segment model — 4. SQLite store —
5. reconciliation — 6. context state + AppState — 7. body rewriting —
8. summarization prompt — 9. compression worker — 10. admin API —
11. Prometheus metrics — 12. integration tests — 13. docs + deployment
