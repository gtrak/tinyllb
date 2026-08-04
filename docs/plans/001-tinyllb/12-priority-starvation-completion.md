# 12 — Priority System + Starvation Protection + Completion Bias

**Phase:** 2 (Agent Scheduling)
**Depends on:** `08`, `10`, `11`.
**Blocks:** `14`.

## Objective

Layer the three cross-scheduler policies from PRD §6.5 / §6.6 on top of the
chosen scheduler:

1. **Priority** (§6.5) — `priority` is admission preference: among eligible
   flows (enough credit), higher `priority` is preferred.
2. **Starvation protection** (§6.5) — if a flow waits longer than
   `starvation_timeout`, it is force-admitted (or boosted to the front) on the
   next slot.
3. **Completion bias** (§6.6) — if `active_generations > target` admissions of
   *new* flows are paused until in-flight completes, avoiding the
   "10 agents @ 10%" anti-pattern.

## Files

| File | Change |
| --- | --- |
| `src/scheduler/priority.rs` | New: priority-aware selector wrapping WFQ/DRR. |
| `src/scheduler/starvation.rs` | New: background watchdog + force-admit. |
| `src/scheduler/completion_bias.rs` | New: gate new-flow admission while active > target. |
| `src/scheduler/mod.rs` | Edit: compose the three policies into the admit path. |
| `src/metrics/queue.rs` | Edit: `llm_flow_starvation_seconds{flow_id}` histogram/gauge. |
| `tests/scheduler_policies.rs` | New: each policy exercised in isolation + combined. |

## Steps

1. `priority`: ordering among eligible flows becomes `priority desc,
   wfq/drr tiebreak`.  Higher-priority flow with ready credit wins admission
   over lower-priority even if its own `service_done/weight` is larger.
2. `starvation_timeout` watchdog: tokio task every `starvation_timeout/4`
   scans waiting flows; any flow with `now - enqueued_at > starvation_timeout`
   is force-admitted by skipping the credit/priority gate (round up the next
   freed slot, or admit immediately if a slot is free).
3. `completion_bias`: add `target_active_flows` (default `max_active_flows`).
   Admission of a request for a flow that is **not currently active** is
   deferred while `active > target_active_flows`.  Requests for a flow
   already active are not gated (so a flow keeps its slot for back-to-back
   requests).
4. Add `llm_flow_starvation_seconds{flow_id}` (PRD §8 Scheduling) — observed
   wait time per flow; force-admit events increment a
   `llm_starvation_force_admits_total{}` counter.
5. Compose carefully to avoid deadlock:
   * completion_bias defers only **new** flows; an active flow's queued
     requests can still be admitted,
   * starvation can force a new flow in over completion_bias (correct by PRD
     intent — starvation protection beats completion bias).
6. Tests (each in isolation, then combined):
   * priority: high-prio flow with multiple ready flows gets served first,
   * starvation: a low-credit low-priority flow force-admitted after
     `starvation_timeout`,
   * completion bias: with `target=2` and 2 active flows, a 3rd distinct flow
     waits until one of the active flows drains (assert via stub backend),
   * combined: completion bias holds normally but yields to starvation force.

## Verification

* `cargo test --test scheduler_policies` green (isolation + combined).
* `llm_flow_starvation_seconds{flow_id}` and `llm_starvation_force_admits_total`
  both present in `/metrics`.
* A simulated 10-flow scenario where each starts a long request then refuses
  to finish shows completion bias kicking in: the proxy admits fewer new
  flows once `active > target`.
