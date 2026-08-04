# 03 — Reverse-Proxy Gateway (OpenAI Routes, Streaming, Errors)

**Phase:** 1 (Basic Queue Proxy)
**Depends on:** `01`
**Blocks:** `04`, `05`.

## Objective

Implement the OpenAI-compatible reverse proxy: forward
`POST /v1/chat/completions`, `POST /v1/completions`, `GET /v1/models` to the
configured vLLM backend (PRD §6.1, §6.8) and return vLLM's responses
unchanged.  Must preserve **streaming** (`text/event-stream` / SSE chunk
passthrough) and **error semantics** (status codes, headers, body).  No
scheduling yet — this issue is the passthrough baseline that later issues
wrap with admission control.

## Files

| File | Change |
| --- | --- |
| `src/gateway/mod.rs` | New: axum router mounting the three OpenAI routes. |
| `src/gateway/proxy.rs` | New: per-request handler that forwards to vLLM and streams the body back. |
| `src/gateway/stream.rs` | New: SSE-aware body that flushes chunks without buffering. |
| `src/gateway/error.rs` | New: forward-only error type; pass backend 4xx/5xx through untouched. |
| `tests/gateway.rs` | New: integration tests against an in-process hyper stub backend. |

## Steps

1. Build a `reqwest::Client` (per-app, with sensible timeouts) at startup;
   store on shared `AppState`.
2. Handler: read incoming request body + headers (strip hop-by-hop headers
   per RFC 7230 §6.1), build a `reqwest::RequestBuilder` to `backend.url +
   original_path` with the same method/headers/body, send it.
3. Non-streaming path: copy `Content-Type`, `Content-Length`, and stream the
   response body to the axum `Response`. Status code preserved verbatim.
4. Streaming path: if `stream:true` in the parsed JSON body **or** the
   backend responds with `text/event-stream`, return a `Body::from_stream`
   wrapping `reqwest`'s `bytes_stream` so each SSE chunk is flushed
   immediately without buffering.  Confirm no buffering in the proxy layer.
5. Error path: backend 4xx/5xx is forwarded **with identical body and
   headers**; only network errors (connection refused, DNS) become `502 Bad
   Gateway`.  Never silently swallow a backend error.
6. Mount routes on the axum router; add `/healthz` passthrough unchanged.
7. Integration tests using `hyper` stub backend:
   * non-stream `chat/completions` returns identical JSON + status,
   * streaming returns identical SSE chunks in order,
   * backend 500 forwards as 500 with same body,
   * backend unreachable -> 502,
   * `/v1/models` GET forwards.

## Verification

* `cargo test --test gateway` passes all cases above.
* Manual: `curl localhost:8080/v1/models` against a real vLLM returns the
  same JSON as `curl vllm:8000/v1/models`.
* Manual streaming: a streaming completion delivers tokens as they arrive
  (no buffering — verify with `--no-buffer` and confirm chunk boundaries).
* Backend error bodies are byte-preserved (assert in tests).
