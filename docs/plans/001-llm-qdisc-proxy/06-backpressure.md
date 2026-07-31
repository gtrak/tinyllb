# 06 — Backpressure (429 + Retry-After; blocking / fail-fast / hybrid)

**Phase:** 1 (Basic Queue Proxy)
**Depends on:** `05`.
**Blocks:** `07`.

## Objective

Implement the backpressure surface from PRD §6.7.  When the queue is under
load, the proxy communicates it one of three configured modes:

* **blocking** — queue indefinitely (default; behavior of `05`).
* **fail-fast** — once queue depth > `max_queue_depth`, return `429 Too Many
  Requests` with `Retry-After` immediately instead of waiting.
* **hybrid** — wait up to `max_wait`; if not admitted in time, return 429.

PRD specifies the modes and the `Retry-After` header; this issue wires the
mode switch into the scheduler and gateway.

## Files

| File | Change |
| --- | --- |
| `src/config/mod.rs` | Edit: add `Backpressure { mode, max_queue_depth, max_wait, retry_after_base }`. |
| `src/scheduler/mod.rs` | Edit: `admit()` returns `AdmitError::Rejected { retry_after }`. |
| `src/scheduler/backpressure.rs` | New: mode logic + `Retry-After` computation. |
| `src/gateway/error.rs` | Edit: map `AdmitError::Rejected` -> `429` with `Retry-After`. |
| `tests/backpressure.rs` | New: each mode exercised. |

## Steps

1. Extend `Backpressure` config from `02` with `max_queue_depth: u32`,
   `max_wait: Duration`, `retry_after_base: Duration`.
2. `admit()` now takes the mode into account:
   * `Blocking`: identical to `05`.
   * `FailFast`: if `depth() > max_queue_depth`, immediately return
     `Rejected{retry_after: retry_after_base * (1 + depth/max_queue_depth)}` —
     a simple linear backoff so heavier queues suggest longer waits.
   * `Hybrid`: race `acquire_permit` against `sleep(max_wait)`; on timeout
     return `Rejected` with `Retry-After: ceil(remaining_estimate)`.
3. Gateway maps `Rejected` to HTTP `429 Too Many Requests` with `Retry-After`
   header (seconds, integer per RFC 7231).  Response body is empty or a small
   JSON `{ "error": "queue full" }` consistent across modes.
4. Add `llm_backpressure_rejections_total{mode}` counter to `04`'s metrics.
5. Tests:
   * `FailFast`: queue 1 over cap -> 429 immediately with `Retry-After` set.
   * `Hybrid`: 429 only after `max_wait`; admit succeeds if a slot frees first.
   * `Blocking`: request waits and eventually forwards.

## Verification

* `cargo test --test backpressure` green for all three modes.
* `curl` shows `HTTP/1.1 429` and `Retry-After:` header on fail-fast and
  hybrid timeouts.
* `llm_backpressure_rejections_total{mode="fail_fast"}` increments appropriately.
* Default config (`blocking`) preserves the FIFO behavior from `05`.
