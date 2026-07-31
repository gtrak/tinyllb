# 18 — Logging / Tracing (Structured, OpenTelemetry-friendly)

**Phase:** Cross-cutting — land early in Phase 1 (`03`), revisit each phase.
**Depends on:** `01`.
**Blocks:** (none — lands alongside `03` and is touched per phase).

## Objective

Stand up the observability story already started with metrics (`04`) by
adding structured tracing.  PRD §8 only specifies Prometheus metrics, but a
daemon meant to run long-term needs logs/traces for incident diagnosis.
`tracing` was pulled in `01`; this issue wires it end-to-end and defines the
conventions every later issue follows.

## Files

| File | Change |
| --- | --- |
| `src/telemetry/mod.rs` | New: init subscriber, JSON formatter, span conventions. |
| `src/main.rs` | Edit: `telemetry::init()` before anything else runs. |
| `src/gateway/proxy.rs` | Edit: per-request span with `flow_id`, `request_id`, `method`, `path`. |
| `src/scheduler/mod.rs` | Edit: span on `admit` (`flow_id`, queued-for, decision). |
| `docs/plans/001-llm-qdisc-proxy/TRACING.md` | New: span field conventions. |

## Steps

1. `tracing_subscriber::fmt()` configured by env (`RUST_LOG`, default `info`,
   `llm_qdisc_proxy=debug`).
2. JSON output mode toggled by `LLM_QDISC_LOG_JSON=1` (for shipping to a log
   aggregator; default stays human-readable).
3. OpenTelemetry export is **scaffolded but commented out**: pulling in
   `opentelemetry-otlp` and running a collector is left for deployment; this
   issue only ensures the code uses `tracing` spans, so an OTLP exporter can
   be added later without rewriting call sites.
4. Span conventions:
   * `request`: `flow_id`, `request_id` (UUID echoed in `X-Request-ID`
     response header), `method`, `path`, `stream=true|false`.
   * `admit`: `flow_id`, `queue_depth_before`, `decision` (`accept|delay|reject`),
     `wait_seconds`, `algorithm`.
   * `backend_forward`: `status`, `tokens` (if known), `duration_ms`.
5. No PII / no prompt bodies logged — only structural fields.
6. `TRACING.md` records the convention; later issues (`08`, `11`, `15`) add
   fields per their domain without bikeshedding.

## Verification

* `cargo run` with `RUST_LOG=debug` shows structured spans for `/healthz` and
  a forwarded request including `flow_id`, `request_id`, `decision`.
* `LLM_QDISC_LOG_JSON=1 cargo run` emits valid JSON-per-line logs parseable by
  `jq`.
* `X-Request-ID` echoed back on a forwarded request's response.
* No prompt bodies appear anywhere in logs (assert via grep on a test run).
