# Context Compression

The proxy intercepts conversation history before it reaches vLLM, maintaining a per-flow transcript and replacing older turns with compact summaries from a background sidecar, extending effective context windows and capping per-flow KV footprint.

## Purpose

Context compression prevents long agentic chains from exhausting GPU KV cache and hitting context-length-exceeded errors, keeping each flow's forwarded context bounded while preserving the information an agent needs to continue.

- Models each flow's conversation as an ordered list of immutable segments: `[Head] [Compressed_1..n] [Live]`.
- Summarizes the oldest compressible chunk of Live turns via a background sidecar `POST /v1/chat/completions` to vLLM when estimated tokens exceed `compress_threshold`.
- Persists transcripts in SQLite so they survive proxy restarts; the proxy is the source of truth for what vLLM sees.
- Fails open: any compression-subsystem error forwards the original request body unchanged.
- Disabled by default; enabled via `context_policy.enabled` in configuration.

## Non-goals

This concept does not define scheduling, backpressure, or admission control behavior.

- Does not perform request scheduling or flow admission — that is [[scheduler]] and [[admission]].
- Does not modify streaming response handling — compression operates on the request body before forwarding.
- Does not support re-compression of already-compressed segments; Compressed segments are immutable.
- Does not guarantee lossless compression; summarization is a lossy semantic reduction by design.
- Does not compress non-chat routes (`/v1/completions`, `/v1/models`); only `/v1/chat/completions` bodies with a `messages` array are eligible.

## Interface

The subsystem exposes a shared state object, a reconciliation entry point, a background worker, an admin inspection API, and Prometheus metrics.

**Shared state.** `ContextState` aggregates the transcript store, token estimator, configuration, per-flow locks, a compression-job channel, the prompt builder, and the metrics handle. It is shared as `Option<Arc<ContextState>>` on [[gateway#Gateway Application State]] — `None` when disabled or on init failure, so the proxy works without compression.

- Per-flow locks serialize concurrent access to the same transcript across reconciliation and compression.
- The compression channel is a bounded mpsc; `trigger_compression` is best-effort and drops on full (re-enqueued on the next request for that flow).

**Reconciliation.** The `reconcile` function is the read+update path: it loads the stored transcript, diffs incoming messages via longest-common-prefix turn detection, appends new turns to Live, detects conversation resets (divergent system prompt), and flags whether compression is needed.

**Body rewriting.** In [[gateway#Reverse Proxy Request Handling]], after flow-ID resolution, `rewrite_messages` reconciles the `messages` array under the per-flow lock, substitutes `[Head + Compressed + Live]`, and enqueues a compression job if needed. Internal sidecar requests (`X-LLM-Internal: compressor`) skip rewriting to prevent recursion.

**Compression worker.** A background tokio task drains the job channel sequentially, acquires the flow lock, builds the summarization prompt, makes a sidecar POST to vLLM, stores the result as a new Compressed segment, and shrinks Live. Retry with exponential backoff.

**Admin API.** See [[api#Admin API Router Assembly]] — `GET /admin/context`, `GET /admin/context/{flow_id}`, `POST /admin/context/{flow_id}/compress`, `DELETE /admin/context/{flow_id}`.

**Metrics.** See [[metrics#Metrics Registry]] — the context compression metric family (`llm_qdisc_context_*`).

## Invariants

The following properties hold for any conformant implementation.

**Segment immutability.** Head and Compressed segments are never modified after creation; only the Live segment is replaced during reconciliation or compression.

**Prefix-cache friendliness.** The forwarded `messages` array is `[Head + Compressed_1..n + Live]`. The prefix `[Head + Compressed]` is byte-identical across requests for the same flow, so vLLM's prefix cache always hits on it; only the new Live suffix needs prefill.

**Fail-open guarantee.** Every error path in `rewrite_messages` returns the original body. The proxy never rejects a request due to compression failure; the only effect of a failure is that uncompressed messages are forwarded.

**Transcript persistence.** Transcripts are stored in SQLite with WAL mode and survive proxy restarts. On startup, flows over `compress_threshold` are re-enqueued for compression.

**Recursion prevention.** Sidecar compression requests carry `X-LLM-Internal: compressor` and are skipped by `rewrite_messages`, preventing infinite compression recursion.

**Turn semantics.** A turn is a user message followed by all non-user messages until the next user message. A leading system message attaches to turn 0. Tool-call and tool-result messages belong to the turn of the preceding user message.

## Constraints

The design has several hard boundaries.

- Compression only runs for `/v1/chat/completions` requests with a `messages` array; other routes are unaffected.
- The sidecar request goes directly to the vLLM backend URL, not through the proxy's own scheduler, to avoid self-referential HTTP.
- The worker processes jobs sequentially; multiple sidecar requests never compete for vLLM slots simultaneously.
- Ephemeral flows aggregate to the `"ephemeral"` metric label to prevent cardinality explosion.
- `ContextState::new` failure (e.g. SQLite cannot open) degrades gracefully to `context = None`; the proxy serves without compression.

## Rationale

The design decisions reflect the operational context of a local-first GPU deployment serving long agentic chains.

**Why segment-based, not sliding-window.** A segment model keeps Head and Compressed byte-identical across requests, which makes vLLM's prefix cache always hit. A sliding window would shift the prefix on every request, defeating the cache.

**Why background sidecar, not inline.** Summarization is a full LLM call that takes seconds; doing it inline would block the user's request. Background compression lets the current request proceed with the existing (possibly over-threshold) context while the summary is produced asynchronously and picked up on the next request.

**Why SQLite.** Transcripts must survive restarts for long-running sessions. SQLite with WAL provides concurrent reads during writes without an external database server — appropriate for a single-node proxy.

**Why fail-open.** Compression is an optimization, not a correctness requirement. Rejecting a user's request because the summarizer failed would turn a background optimization into a reliability liability.

**Why disabled by default.** Compression adds a sidecar LLM call per over-threshold flow, consuming GPU cycles. Operators opt in after confirming the tradeoff is acceptable for their workload.

## Related

Concepts and source artifacts associated with context compression.

- [[config#Configuration Contract]] — `ContextPolicy` configuration fields and validation
- [[gateway#Gateway Application State]] — `AppState.context` optional field
- [[gateway#Reverse Proxy Request Handling]] — `rewrite_messages` insertion point
- [[api#Admin API Router Assembly]] — context admin endpoints
- [[metrics#Metrics Registry]] — context compression metric family
- [[src/context/mod.rs#ContextState]] — shared compression state
- [[src/context/mod.rs#CompressionJob]] — background job descriptor
- [[src/context/estimator.rs#TokenEstimator]] — token count estimation
- [[src/context/segment.rs#Segment]] — transcript segment type
- [[src/context/segment.rs#SegmentKind]] — Head / Compressed / Live
- [[src/context/segment.rs#Transcript]] — per-flow transcript
- [[src/context/store.rs#TranscriptStore]] — persistence trait
- [[src/context/store.rs#SqliteStore]] — SQLite implementation
- [[src/context/reconcile.rs#reconcile]] — reconciliation entry point
- [[src/context/prompt.rs#PromptBuilder]] — summarization prompt construction
- [[src/context/compressor.rs#CompressionWorker]] — background worker

# Segment Model and Transcript Types

Core data structures modeling each flow's conversation as an ordered list of immutable segments, used by the store, reconciliation, and compression worker.

## Segment kinds

A transcript is `[Head] [Compressed_1..n] [Live]`:

- **Head** — first `head_keep_turns` turns (system prompt + earliest exchanges), kept verbatim. Prefix-cache anchor; never changes after creation.
- **Compressed** — a `{role: "system", content: "Summary of turns X-Y: ..."}` message produced by the sidecar. Immutable once written; costs a single prefill then stays cached.
- **Live** — most recent `live_keep_turns` turns, kept verbatim. Grows turn-by-turn; the oldest chunk is folded into a new Compressed segment when the threshold is exceeded.

## Turn boundary detection

A turn boundary is at the start of each `role: "user"` message. A leading system message attaches to turn 0 (not a boundary). `find_turn_boundaries` returns `[0]` plus the indices of the 2nd, 3rd, ... user messages.

Tool-call and tool-result messages belong to the preceding user's turn.

## Split semantics

`split_messages_at_turns` partitions a message array into `(head, middle, live)` by turn count. When total turns ≤ `head_keep_turns + live_keep_turns`, the middle is empty and Live shrinks (Head takes precedence).

## Related

Source references for the segment model.

- [[src/context/segment.rs#Segment]]
- [[src/context/segment.rs#SegmentKind]]
- [[src/context/segment.rs#Transcript]]
- [[src/context/segment.rs#find_turn_boundaries]]
- [[src/context/segment.rs#split_messages_at_turns]]

# Token Estimation

Estimates token counts for text and message arrays using the model's HF tokenizer, with a character-ratio heuristic fallback when no tokenizer is configured.

## Estimation modes

Two modes are available, selected by whether a tokenizer path is configured and loadable.

- **Tokenizer mode**: when `tokenizer_path` points to a valid `tokenizer.json`, exact BPE token counts are produced via `tokenizers::Tokenizer::encode`.
- **Heuristic mode**: when no tokenizer is configured or loading fails, the fallback is `(len.max(1) * 10 + 3) / 32` (chars / ~3.2, Qwen BPE ratio). Empty string yields 0.

## Message overhead

Each message incurs +4 tokens (role tag + structural framing). Multimodal array content counts only text parts; image/video parts are skipped.

Missing or null content contributes 0 text tokens but still incurs the per-message overhead.

## Related

Source references for token estimation.

- [[src/context/estimator.rs#TokenEstimator]]
- [[src/context/estimator.rs#TokenEstimator#estimate_text]]
- [[src/context/estimator.rs#TokenEstimator#estimate_messages]]

# Reconciliation

The read+update path matching an incoming `messages` array against the stored transcript, detecting new turns and resets, and flagging compression need.

## Algorithm

The reconciliation algorithm proceeds in five steps, from empty-message short-circuit through transcript creation, diffing, and compression-trigger check.

1. **Empty messages**: return an empty result immediately.
2. **No existing transcript**: split incoming at `head_keep_turns` / `live_keep_turns`, create Head + Live segments, save, return forwarded messages.
3. **Existing transcript**: find new turns via longest-common-prefix matching. If the first message diverges from stored Head, detect a reset and create a fresh transcript.
4. **Append new turns** to Live, recompute token totals, save.
5. **Compression trigger**: if `total_est_tokens > compress_threshold`, set `needs_compression` and compute `compress_turn_range`.

## Divergence handling

Three divergence cases are handled differently, from full conversation reset to conservative partial-match acceptance.

- **Full reset** (first message differs): old transcript deleted, fresh one created.
- **Partial divergence** (prefix matches then diverges mid-conversation): warning logged, only tail turns accepted conservatively.
- **Incoming shorter than stored**: treated as client-side truncation; no new turns extracted.

## Related

Source references for reconciliation.

- [[src/context/reconcile.rs#reconcile]]
- [[src/context/reconcile.rs#ReconcileResult]]
- [[src/context/reconcile.rs#find_new_turns]]

# Background Compression Worker

A background tokio task consuming compression jobs, making sidecar summarization requests to vLLM, and storing summaries as new Compressed segments.

## Processing model

The worker processes jobs sequentially to avoid competing sidecar requests for vLLM slots. Each job acquires the per-flow lock for its entire duration (including the sidecar call), blocking only other requests for the same flow.

## Sidecar request

A non-streaming `POST /v1/chat/completions` to the vLLM backend with `temperature: 0.3`, `max_tokens: summary_max_tokens`, and the summarization prompt. It carries `X-LLM-Internal: compressor` and `X-LLM-Flow-ID: compressor:{flow_id}`.

Retry with exponential backoff (1s, 2s, 4s) up to `compression_retries`.

## Segment mutation

On success, the worker creates a Compressed segment (summary message + original raw messages for audit), shrinks Live by removing the compressed turns, saves both, and updates meta. On failure after all retries, Live is unchanged.

## Related

Source references for the compression worker.

- [[src/context/compressor.rs#CompressionWorker]]
- [[src/context/compressor.rs#CompressionWorker#run]]
- [[src/context/compressor.rs#CompressionWorker#process_job]]

# Summarization Prompt

Builds the messages array for the sidecar summarization request, with a default template preserving code references and task state, plus optional custom template support.

## Default template

The default system prompt instructs the summarizer to preserve code snippets, file paths, decisions, factual context, tool-call outcomes, and task state, while compressing redundancy and verbose code blocks.

It uses `{start}`, `{end}`, and `{max_tokens}` placeholders.

## Message serialization

Turns are formatted as a readable transcript (`[role]: content` blocks) with truncation: messages over 2000 chars get `[...truncated]`; tool-call arguments to 200 chars; tool results to 500 chars.

## Related

Source references for the summarization prompt.

- [[src/context/prompt.rs#PromptBuilder]]
- [[src/context/prompt.rs#build_summarization_prompt]]

# Transcript Store

Persistent transcript storage backed by SQLite via `sqlx`, providing CRUD for segments and metadata with WAL mode for concurrent reads during writes.

## Schema

Two tables: `segments` (primary key `flow_id, segment_idx`) storing raw and summary messages as JSON text, and `transcript_meta` (primary key `flow_id`) storing aggregate token counts and turn tallies.

## Conventions

The store follows several implementation conventions for query strategy, concurrency, and segment indexing.

- Runtime queries (`sqlx::query`, not macros) to avoid compile-time DB requirements.
- WAL mode for concurrent reads; in-memory databases silently ignore the WAL pragma.
- `update_live_segment` deletes the existing Live and inserts a new one with computed `segment_idx`.
- Migrations embedded at compile time via `sqlx::migrate!`.

## Related

Source references for the transcript store.

- [[src/context/store.rs#TranscriptStore]]
- [[src/context/store.rs#SqliteStore]]
- [[src/context/store.rs#TranscriptMeta]]
