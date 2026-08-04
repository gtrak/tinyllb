# 06 — Tests: unit, integration, end-to-end

**Phase:** 4 (verification)
**Depends on:** `02`, `03`, `04`, `05`.
**Blocks:** `07`.

## Objective

End-to-end verification that the heuristic improves interactive
scheduling, header overrides land, and the existing starvation safety
net still fires. Three new test files plus targeted additions to the
existing suites.

## Files

| File | Change |
| --- | --- |
| `tests/priority_heuristic.rs` | NEW (already started in 02): exhaustive classify table. |
| `tests/priority_header.rs` | NEW (already started in 03): header matrix. |
| `tests/priority_live.rs` | NEW: end-to-end scheduler test with two fake flows. |
| `tests/flow_identify.rs` | EDIT: update consumers of `resolve` to use `.flow_id`. |

## Steps

### `tests/priority_heuristic.rs`

1. (`02` contributed the unit cases listed there.) Add coverage for
   the gap boundaries: assert `== background_gap_max` is classified
   as `background`, `== interactive_gap_min` is classified as
   `interactive`, and a one-second-difference gap inside the agent
   band stays `agent`.
2. Test the sample-window eviction: with `sample_window = 3`,
   schedule 5 fast arrivals then 1 slow arrival. Assert the rolling
   median is dominated by the slow arrival and the flow leaves
   `background` once the slow sample fills the window.
3. Property test (optional, if `proptest` already exists in the
   crate): for arbitrary sequences of `n >= min_samples`, the
   returned priority is one of `{10, 50, 100}` and consistent with
   the median gap.

### `tests/priority_header.rs`

1. (`03` contributed the header matrix.) Add a test asserting that
   re-sending the same `X-LLM-Priority` header on subsequent
   requests is idempotent (priority remains the same,
   `priority_source` stays `1`).
2. Add a test where flow A is pinned via header to `background` and
   flow B is unpinned: assert the heuristic still runs for flow B
   while flow A stays pinned. Verifies per-flow override isolation.

### `tests/priority_live.rs`

The crucial scheduling test. Build a real `Scheduler` with a tiny
`max_active_flows=2`, run a tokio task per flow, and assert that an
interactive flow wins slots over a background flow.

1. Span two artificial flows:
   - `interactive_test_flow`: cadence of one synthetic `admit()`
     every 20s (median ≥ `interactive_gap_min=30s`? Set policy to
     `interactive_gap_min: 15s` for test brevity).
   - `batch_test_flow`: cadence of one `admit()` every 100ms (well
     below `background_gap_max`).
   - Configure policy: `interactive_gap_min: 15s,
     background_gap_max: 1s, sample_window: 10, min_samples: 3`.
2. Both flows use a fake `work_unit = 1.0`. The backend is replaced
   with a `tokio::Notify`-driven dummy so a test thread controls
   when slots free up.
3. Drive 5 admit cycles from each flow. Collect the order of
   admitted `flow_id`s in a `Vec<FlowId>`.
4. Assert: once both flows have >= `min_samples` arrivals, the
   interactive flow is admitted at the next slot opportunity. Over
   the 5+ alternating slot releases, the interactive flow is
   selected at least 80% of the time when both are waiting. (Exact
   ratio depends on DRR credit fairness; keep it coarse to avoid
   flakiness.)
5. Starvation regression test:
   - Pin `batch_test_flow` to `interactive` via header; pin
     `interactive_test_flow` to `background`.
   - Drive admits so `batch` is always waiting.
   - Set `starvation_timeout = 500ms` for the test.
   - Assert `background` flow is force-admitted within
     `starvation_timeout + small_slack` (e.g. 600ms), proving the
     starvation mechanism still trumps priority.

### `tests/flow_identify.rs`

Update existing call sites to `resolve(...).flow_id`. Add one new
case verifying the header is not required (existing behavior
preserved) and one verifying that `X-LLM-Flow-ID` still wins over
`X-LLM-Priority` (i.e. the priority header does not interfere with
flow identity resolution). This is mostly a defensive fill-in for
the new return type.

### Coverage command

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all
cargo test --test priority_heuristic -- --nocapture
cargo test --test priority_header -- --nocapture
cargo test --test priority_live -- --nocapture
```

All must be green before opening the merge of this plan.

## Verification

- Every success criterion listed in `PLAN.md` has a corresponding
  passing test in this issue's files.
- No flaky timing tests. Where timing assertions are unavoidable,
  use `tokio::time::pause()` + `advance()` to remove wallclock
  dependencies.
- Coverage: at minimum, every branch of
  `CadenceRegistry::classify_and_apply` runs in at least one test
  (cold-start, heuristic-active, override-blocked, hysteresis).

## Notes

- The "80% interactive wins" assertion is intentionally loose. The
  DRR scheduler still obeys fairness over time; with such a stark
  gap difference the priority dominates, but a 100% assertion
  would be fragile.
- All timing in tests uses `PriorityPolicy { min_samples: 3,
  sample_window: 10, ... }` so the heuristic engages within the
  first few cycles.
