# 02 — Cadence module + classify logic

**Phase:** 2 (heuristic core)
**Depends on:** `01`.
**Blocks:** `04`.

## Objective

Implement the request-cadence heuristic. Each `admit()` records an
arrival timestamp for the flow; `classify()` inspects the rolling
window of arrivals and produces a numeric priority (one of
`interactive` / `agent` / `background`). Provide a top-level
`classify_and_apply(flow, flow_id)` that updates `Flow.priority` in
place when the heuristic is enabled and no override is set.

## Files

| File | Change |
| --- | --- |
| `src/flow/cadence.rs` | Implement `record_arrival`, `median_gap`, `classify`, `classify_and_apply`. |
| `src/flow/mod.rs` | Re-export public symbols; thread `PriorityPolicy` into `CadenceRegistry`. |
| `tests/priority_heuristic.rs` | NEW: unit tests exercising the classify table at boundary gaps. |

## Steps

1. Extend `CadenceRegistry::new` to accept a `PriorityPolicy` and
   store it. Add `Arc<PriorityPolicy>` as a field.

2. `Cadence::record_arrival(now: Instant, window: usize)`:
   - `self.arrivals.push_back(now)`.
   - If `self.arrivals.len() > window`, `pop_front()` until bounded.

3. `Cadence::median_gap() -> Option<Duration>`:
   - If fewer than 2 samples, return `None`.
   - Otherwise compute consecutive deltas (`arrivals[i+1] -
     arrivals[i]`) over the last `min(arrivals.len(), window)`
     entries, and return the median (or middle-of-two for even
     counts — implementation detail; just be deterministic).

4. `Cadence::classify(policy: &PriorityPolicy) -> Option<u32>`
   returns `Option<priority_value>`:
   - If `arrivals.len() < policy.min_samples`, return `None` (cold
     start, leave priority alone).
   - Otherwise let `gap = median_gap()`:
     - `gap <= policy.background_gap_max` → `background` (10).
     - `gap >= policy.interactive_gap_min` → `interactive` (100).
     - in between → `agent` (50, the default).
   - Pull the numeric values from a small table constant
     (`PriorityValue::INTERACTIVE` etc.), or take them as constructor
     params to avoid coupling to the `Priorities` config. The latter
     is cleaner: pass `Priorities` into `CadenceRegistry::new` and
     store it.

   Final signature:
   ```rust
   pub fn classify(&self, policy: &PriorityPolicy, classes: &Priorities) -> Option<u32>
   ```

5. `CadenceRegistry::classify_and_apply(flow: &Flow, flow_id: &FlowId)`:
   ```rust
   pub fn classify_and_apply(&self, flow: &Flow, flow_id: &FlowId) {
       // Honor explicit overrides (1 = header, 2 = admin) — do NOT
       // overwrite. Only the heuristic may write when source == 0.
       if flow.priority_source() != 0 {
           return;
       }
       if !self.policy.enabled {
           return;
       }
       let entry = self.inner.entry(flow_id.clone())
           .or_insert_with(|| Cadence::new());
       let new = match entry.classify(&self.policy, &self.classes) {
           Some(v) => v,
           None => return,  // keep current priority
       };
       // Hysteresis: only demote an interactive flow if it has been
       // interactive for at least policy.sample_window samples AND
       // the new class is strictly lower. Promote always.
       let current = flow.priority();
       if new < current && current == self.classes.interactive {
           // Don't thrash: require sustained batch cadence before demoting.
           // (See Hysteresis section in PLAN.md risks.)
           // Implemented by checking that the *last 3* gaps are all
           // fast — if any are slow, keep interactive.
           if !entry.last_k_gap_all_le(self.policy.background_gap_max, 3) {
               return;
           }
       }
       flow.set_priority(new);
   }
   ```

6. `Cadence::last_k_gap_all_le(threshold: Duration, k: usize) -> bool`:
   helper computing the last `k` consecutive gaps and returning
   `true` only if all of them are `<= threshold`. Used by the
   hysteresis guard in 5.

7. `CadenceRegistry::record_arrival(flow_id, now)`:
   public entrypoint called by the scheduler on every `admit()` (see
   04). Delegates to the per-flow `Cadence`.

8. Unit tests in `tests/priority_heuristic.rs`:
   - `cold_start_keeps_default`: < `min_samples` samples ⇒ classify
     returns `None` ⇒ priority unchanged.
   - `rapid_fire_demotes_to_background`: 5 arrivals @ 0.5s gap, median
     ≤ `background_gap_max` ⇒ returns background.
   - `slow_paced_promotes_to_interactive`: 5 arrivals @ 60s gap ⇒
     returns interactive.
   - `medium_keeps_agent`: 5 arrivals @ 10s gap ⇒ returns agent.
   - `hysteresis_blocks_one_shot_demotion`: a flow currently at
     interactive that then sees one fast burst (≤ 2s) but the median
     has slow gaps in the last 3 ⇒ stays interactive. Then a sustained
     run of fast gaps (last 3 all fast) ⇒ demotes.
   - `override_blocks_classify`: when `flow.priority_source()` != 0,
     `classify_and_apply` does nothing even with a long history.

## Verification

- `cargo test --test priority_heuristic` green.
- `cargo clippy --all-targets -- -D warnings` clean.
- Property: `classify_and_apply` never overwrites an explicitly
  pinned priority (assert in unit test using a tiny harness).

## Notes

- The cutoff semantics use `<=` for background and `>=` for
  interactive, with `>` / `<` falling into the default band. Tests
  must cover the exact edges (`== background_gap_max` is background;
  `== interactive_gap_min` is interactive).
- The cadence window is FIFO eviction — newest samples displace
  oldest. Do not sort the deque.

