# 09 — Flow API (POST /flows) + GET /queue Status

**Phase:** 2 (Agent Scheduling)
**Depends on:** `08`.
**Blocks:** `12` (queue visibility used by tests).

## Objective

Expose the optional administrative API from PRD §9:
* `POST /flows` — register (or update) a flow with explicit `weight` /
  `priority`,
* `GET /queue` — return current queue depth, active count, and per-flow
  waiting position.

This makes the scheduler observable and controllable without restarts.

## Files

| File | Change |
| --- | --- |
| `src/api/mod.rs` | New: admin router mounted at root (not under `/v1/...`). |
| `src/api/flows.rs` | New: `POST /flows` handler. |
| `src/api/queue.rs` | New: `GET /queue` handler. |
| `src/flow/mod.rs` | Edit: `register(flow)` upsert; `queue_snapshot()`. |
| `tests/api_flows_queue.rs` | New: register, update, list, shape matches PRD §9. |

## Steps

1. `POST /flows` body per PRD §9:
   ```json
   { "id": "agent1", "weight": 5, "priority": 50 }
   ```
   Upsert into the registry (`08`).  Validation: `weight > 0`,
   `priority` in `[0,100]`.  Returns `201 Created` (new) or `200 OK` (update).
2. `GET /queue` response per PRD §9:
   ```json
   {
     "active": 4,
     "waiting": 12,
     "flows": [ { "id": "coder", "position": 2 } ]
   }
   ```
   `flows` lists only flows currently **waiting** (active flows aren't
   queued), ordered by queue position.  `position` is 1-indexed within the
   whole queue.
3. Mount under `/flows`, `/queue` (not `/v1/...` — these are control-plane
   endpoints; in a later plan they could be gated by token, but auth is a
   non-goal this plan per PRD §3).
4. Tests:
   * register new flow -> registry shows it,
   * update existing flow's weight -> registry reflects,
   * invalid weight/priority -> `400` with helpful message,
   * `GET /queue` under load reflects the in-flight count and waiting flows
     (drive via stub backend holding requests).

## Verification

* `cargo test --test api_flows_queue` green.
* `curl -X POST localhost:8080/flows -d '{"id":"a","weight":5,"priority":50}'`
  -> `201`; same call again -> `200`.
* During a load test: `curl localhost:8080/queue` shows `active`, `waiting`,
  and `flows[].position` consistent with the stub backend's held requests.
