# 05 — FIFO Queue with max_active_flows Admission

**Phase:** 1 (Basic Queue Proxy)
**Depends on:** `02`, `03`, `04`.
**Blocks:** `06`, `07`.

## Objective

Implement the Phase 1 MVP scheduler (PRD §6.4 "MVP: FIFO with Flow Limits",
§6.3 "Maximum active generations").  Requests beyond `max_active_flows` wait
in an in-process FIFO queue; admission only releases a request to the gateway
when an active slot frees.  At this stage there is one global queue (no flow
grouping yet — that lands in `08`/`10`/`11`).

## Files

| File | Change |
| --- | --- |
| `src/scheduler/mod.rs` | New: `Scheduler` trait + `Admit` gate holding `Semaphore`/active set. |
| `src/scheduler/fifo.rs` | New: FIFO impl; per-request `QueueTicket`. |
| `src/gateway/proxy.rs` | Edit: wrap request in `scheduler.admit().await?` before forwarding. |
| `src/metrics/queue.rs` | Edit: update `llm_queue_depth`, `llm_queue_wait_seconds`, `llm_active_flows` from scheduler events. |
| `tests/scheduler_fifo.rs` | New: admit/release, queue above cap, head-of-queue released on finish. |

## Steps

1. `Admit` gate: `tokio::sync::Semaphore` sized to `max_active_flows`.
   `admit()` returns a `QueueTicket` that RAII-releases the permit on drop,
   guaranteeing slot release on success, error, **and** client disconnect.
2. `FifoScheduler`: wraps `Admit`; records queue wait time (`Instant` to
   `permit_acquired`) into `llm_queue_wait_seconds`, exposes `depth()` for
   `llm_queue_depth`.
3. Gateway edit: `let _ticket = scheduler.admit().await?; forward(request).await`.
   The `?` here is only for the hard-cancel path (fail-fast modes in `06`); in
   blocking mode `admit()` simply awaits.
4. Update `llm_active_flows` gauge: `+1` on permit acquired, `-1` on release.
5. Tests:
   * `max=2`, fire 3 requests: 2 forward immediately, 3rd waits, released
     when one completes,
   * wait-time metric recorded > 0 for the queued request,
   * panic / early-return / channel drop in the forwarded task correctly
     releases the permit (no leak) — use `tokio::task` + `Drop` guard.
6. Keep the queue **in-memory** and **ephemeral** (documented limitation —
   persistent queues are a future plan per PRD §13).

## Verification

* `cargo test --test scheduler_fifo` green.
* With `max_active_flows=2`, a 3-concurrent-request load test shows exactly
  2 in flight at the backend (assert via stub backend's active counter).
* `llm_active_flows` never exceeds `max_active_flows` in a scrape during load.
* No permit leak under error/cancel paths (long-running stress test with
  random disconnects shows `llm_active_flows` returns to 0 after drain).
