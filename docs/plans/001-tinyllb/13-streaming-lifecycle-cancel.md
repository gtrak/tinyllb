# 13 — Streaming Lifecycle + Cancellation + Credit Restoration

**Phase:** 2 (Agent Scheduling)
**Depends on:** `11`, `12`.
**Blocks:** `14`.

## Objective

Harden the streaming and cancellation behavior from PRD §6.8 / §6.9 so that
scheduler resources (admission slot **and** flow credit) are correctly
released on every completion path:

* request_completed,
* client disconnect,
* timeout,
* explicit cancel (client cancels its own request).

On cancel, the consumed credit must be **restored** (PRD §6.9) so the flow
isn't penalized for partial work it never delivered.

## Files

| File | Change |
| --- | --- |
| `src/scheduler/lifecycle.rs` | New: per-request `LifecycleGuard` RAII handle. |
| `src/gateway/stream.rs` | Edit: detect disconnect; emit `request_cancelled`. |
| `src/scheduler/drr.rs` | Edit: `restore_credit(flow, consumed)` on cancel. |
| `tests/lifecycle_cancel.rs` | New: cancel paths restore credit, release slot. |

## Steps

1. LifecycleGuard owns the admission permit + the `consumed_cost` snapshot.
   On Drop, release the permit and report `consumed_cost` to the scheduler so
   DRR accounting matches actual delivered work (not estimated).
2. Event surface per PRD §6.8: `request_started`, `token_received`,
   `request_completed`, `request_cancelled`.  Emit them on a tracing span
   and a metric counter (`llm_request_events_total{event=...}`).
3. Disconnect detection: `axum` body is a `stream`; if the client connection
   closes, the response body future returns `Err`.  Catch, emit
   `request_cancelled`, and `restore_credit` (we did deliver some tokens before
   the disconnect — restore the *unused* portion of the estimated cost).
4. Timeout: if `request_timeout` config set, cancel forwarded request after
   duration via `tokio::time::timeout`.  Same restore path.
5. Explicit cancel: a `DELETE /v1/requests/{id}` route cancels an in-flight
   request (optional for V1; track request by an `X-Request-ID` echoed back
   in a response header).  Restores credit and releases slot.
6. Credit restoration rule (`11`):
   * On complete: `credit -= tokens_delivered` (actual usage).
   * On cancel: `credit -= tokens_delivered` (delivered so far), the
     remaining `cost - tokens_delivered` is **not** consumed (restored).
   * Use the `usage` SSE frame's `completion_tokens` where available; if
     absent, fall back to the original `max_tokens` estimate (documented
     imprecision; Phase 3 closes it).

## Verification

* `cargo test --test lifecycle_cancel` green:
  * client disconnect mid-stream releases the admission slot (`llm_active_flows`
    drops back),
  * credit restored to (close to) pre-request value after a near-immediate
    cancel,
  * timeout cancels and restores,
  * explicit DELETE cancels.
* `llm_request_events_total{event="request_cancelled"}` increments on
  disconnect; `event="request_completed"` on success.
* Long-running stress test from `05` re-run with random disconnects: no
  permit leak, no permanent credit deficit on any flow.
