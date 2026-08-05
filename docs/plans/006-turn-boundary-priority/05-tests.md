# 05 — Test Rewrite

**Parent:** `PLAN.md`  
**Depends on:** `04-scheduler-admit.md`

## Objective

Replace the median-gap test suite with state-machine tests that exercise
turn-boundary detection, the promotion/demotion ladder, reactive
re-promotion, and the gap patterns the median model got wrong.

## Files

| File | Change |
|---|---|
| `tests/priority_heuristic.rs` | Full rewrite: unit tests for the state machine via `CadenceRegistry` |
| `tests/priority_live.rs` | Full rewrite: end-to-end tests via `Scheduler::admit_with_turn_boundary` |

## Steps

### 1. `tests/priority_heuristic.rs` — helpers

Replace `make_registry` to construct a `PriorityPolicy` with the new fields:

```rust
fn make_registry() -> CadenceRegistry {
    let policy = PriorityPolicy {
        enabled: true,
        idle_gap_threshold: Duration::from_secs(30),
        agentic_suspected_threshold: 5,
        agentic_confirmed_threshold: 12,
    };
    let classes = Priorities {
        interactive: 100,
        agent: 50,
        background: 10,
    };
    CadenceRegistry::new(Arc::new(policy), Arc::new(classes))
}
```

Replace `record_uniform_arrivals` with two helpers — one for continuous
(tool) arrivals and one for turn-boundary arrivals:

```rust
/// Record `count` arrivals with uniform `gap`, all as non-turn-boundary
/// (role:tool / intra-turn).
fn record_tool_arrivals(
    registry: &CadenceRegistry,
    flow_id: &FlowId,
    gap: Duration,
    count: usize,
) {
    let t0 = Instant::now();
    for i in 0..count {
        registry.record_arrival(flow_id, t0 + gap * i as u32, false);
    }
}

/// Record `count` arrivals with uniform `gap`, all as turn boundaries
/// (role:user).
fn record_user_arrivals(
    registry: &CadenceRegistry,
    flow_id: &FlowId,
    gap: Duration,
    count: usize,
) {
    let t0 = Instant::now();
    for i in 0..count {
        registry.record_arrival(flow_id, t0 + gap * i as u32, true);
    }
}

/// Record a single arrival at a specific time + turn-boundary flag.
fn record_at(
    registry: &CadenceRegistry,
    flow_id: &FlowId,
    t: Instant,
    is_turn_boundary: bool,
) {
    registry.record_arrival(flow_id, t, is_turn_boundary);
}
```

### 2. `tests/priority_heuristic.rs` — test cases

Each test calls `record_*` then `classify_and_apply` and asserts the flow's
priority.

| Test name | Pattern | Assert |
|---|---|---|
| `cold_start_is_interactive` | 1 user arrival, gap N/A (first) | priority 100 (Cold) |
| `cold_stays_interactive_under_threshold` | 3 user arrivals, 10s gaps (< 30s threshold → fast turn boundaries, counter resets, no idle chunk) | priority 100 (Cold) |
| `continuous_tool_demotes_to_suspected` | 5 tool arrivals, 1s gaps | priority 50 (AgenticSuspected) |
| `continuous_tool_demotes_to_confirmed` | 12 tool arrivals, 1s gaps | priority 10 (AgenticConfirmed) |
| `idle_chunk_promotes_to_interactive` | 5 tool arrivals (→ AgenticSuspected), then 1 user arrival with 45s gap | priority 100 (Interactive) |
| `tool_gap_does_not_promote` | 5 tool arrivals, then 1 tool arrival with 45s gap | priority 50 (stays AgenticSuspected — tool gap is not a turn boundary) |
| `fast_turn_boundary_resets_counter` | 4 tool arrivals (counter=4, just under threshold 5), then 1 user arrival with 5s gap (fast turn boundary, counter resets to 0), then 4 more tool arrivals (counter=4) | priority 100 (still Cold — never hit threshold) |
| `interactive_demotes_after_continuous_run` | 1 user arrival (Cold), then 5 tool arrivals | priority 50 (AgenticSuspected) |
| `interactive_cycles_back` | idle chunk → Interactive, then 12 tool arrivals → AgenticConfirmed, then idle chunk → Interactive | priority 100 at start and end, 10 in the middle |
| `header_override_blocks_heuristic` | flow with `priority_source = 1`, then tool arrivals | priority unchanged (override honored) |
| `disabled_heuristic_no_change` | `enabled: false`, then user arrivals | priority stays at default (50) |
| `burst_then_idle_pattern` | gaps `[1,1,1,45,1,1,1,60]` where 45s and 60s are `role:user`, rest are `role:tool` | priority 100 (Interactive — two idle chunks observed) |

For the `burst_then_idle_pattern` test, use explicit timestamps:

```rust
let t0 = Instant::now();
record_at(&reg, &id, t0, true);                                   // user
record_at(&reg, &id, t0 + Duration::from_secs(1), false);         // tool, gap 1s
record_at(&reg, &id, t0 + Duration::from_secs(2), false);         // tool, gap 1s
record_at(&reg, &id, t0 + Duration::from_secs(3), false);         // tool, gap 1s
record_at(&reg, &id, t0 + Duration::from_secs(48), true);         // user, gap 45s → idle chunk
record_at(&reg, &id, t0 + Duration::from_secs(49), false);        // tool, gap 1s
record_at(&reg, &id, t0 + Duration::from_secs(50), false);        // tool, gap 1s
record_at(&reg, &id, t0 + Duration::from_secs(51), false);        // tool, gap 1s
record_at(&reg, &id, t0 + Duration::from_secs(111), true);       // user, gap 60s → idle chunk
reg.classify_and_apply(&flow, &id);
assert_eq!(flow.priority(), 100, "Two idle chunks → Interactive");
```

### 3. `tests/priority_live.rs` — end-to-end tests

These go through `Scheduler::admit_with_turn_boundary` and verify that the
DRR scheduler actually favors the interactive flow when contention exists.

Key test — `interactive_flow_wins_over_agentic`:

1. Create two flows: `user-flow` and `tool-flow`.
2. Pin a "holder" flow to keep all 4 slots busy.
3. Enqueue `tool-flow`: 12 admits with `is_turn_boundary=false`, 1s gaps →
   `AgenticConfirmed` (priority 10).
4. Enqueue `user-flow`: 1 admit with `is_turn_boundary=true`, 45s gap after
   the previous → `Interactive` (priority 100).
5. Release the holder slot.
6. Assert `user-flow` is admitted before `tool-flow` (priority 100 > 10).

Key test — `reactive_promotion`:

1. Enqueue a flow with 12 tool admits → `AgenticConfirmed` (priority 10).
2. Send one user admit with 45s gap → `Interactive` (priority 100).
3. Assert the flow's priority is now 100 and it wins the next slot over a
   fresh `AgenticConfirmed` flow.

Key test — `cold_start_optimistic`:

1. A brand-new flow's first admit should get priority 100 (Cold), and it
   should win a slot over an `AgenticConfirmed` flow (priority 10).

Use the existing `WORK_UNIT` constant and scheduler construction helpers
from the current `priority_live.rs` — those don't change. Only the admit
calls change (use `admit_with_turn_boundary`).

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo test --all --test priority_heuristic
cargo test --all --test priority_live
cargo test --all
```
