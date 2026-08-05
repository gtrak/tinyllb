# 02 — Cadence Module Rewrite

**Parent:** `PLAN.md`  
**Depends on:** `01-config-schema.md`

## Objective

Replace the median-gap classifier in `src/flow/cadence.rs` with a
turn-boundary-aware state machine. The `Cadence` struct no longer stores a
VecDeque of arrival timestamps; it tracks a small fixed set of state fields
and transitions reactively as turn-boundary idles and continuous arrivals
are observed.

## Files

| File | Change |
|---|---|
| `src/flow/cadence.rs` | Full rewrite: `Cadence` struct, `CadenceRegistry` methods, state enum |

## Steps

### 1. Define the state enum

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CadenceState {
    /// New flow, no evidence yet. Optimistic: priority = interactive.
    Cold,
    /// ≥1 turn-boundary idle observed. Priority = interactive.
    Interactive,
    /// Continuous arrivals (no idle) past agentic_suspected_threshold.
    /// Priority = agent.
    AgenticSuspected,
    /// Continuous arrivals past agentic_confirmed_threshold.
    /// Priority = background.
    AgenticConfirmed,
}

impl CadenceState {
    /// Map state to numeric priority using the configured Priorities.
    pub fn priority(&self, classes: &Priorities) -> u32 {
        match self {
            CadenceState::Cold => classes.interactive,
            CadenceState::Interactive => classes.interactive,
            CadenceState::AgenticSuspected => classes.agent,
            CadenceState::AgenticConfirmed => classes.background,
        }
    }
}
```

### 2. Replace the `Cadence` struct

```rust
pub struct Cadence {
    /// Timestamp of the last arrival (for gap computation).
    /// `None` until the first arrival.
    last_arrival: Option<Instant>,
    /// Consecutive arrivals since the last turn boundary (idle or fast).
    /// Resets to 0 on any role:user request. Increments on role:tool
    /// or other non-turn-boundary arrivals.
    continuous_arrival_count: u32,
    /// Current state-machine state.
    state: CadenceState,
}

impl Cadence {
    pub fn new() -> Self {
        Self {
            last_arrival: None,
            continuous_arrival_count: 0,
            state: CadenceState::Cold,
        }
    }
}
```

Remove: the `VecDeque<Instant>`, `record_arrival(now, window)`,
`median_gap()`, `classify()`, and `last_k_gap_all_le()` methods. None of
these are used outside this module (verify with `rg` before deleting).

### 3. Rewrite `CadenceRegistry::record_arrival`

New signature:

```rust
/// Record an arrival and update the state machine. Returns the gap
/// since the previous arrival (for the histogram), or `None` for the
/// first arrival.
///
/// `is_turn_boundary` is true when the current request's last message
/// has `role: "user"` or `"system"` (or is non-JSON / non-chat — the
/// optimistic default). It is false for `role: "tool"` or `"assistant"`.
pub fn record_arrival(
    &self,
    flow_id: &FlowId,
    now: Instant,
    is_turn_boundary: bool,
) -> Option<Duration> {
    let mut entry = self.inner.entry(flow_id.clone()).or_default();
    let prev_gap = entry.last_arrival.map(|last| now.duration_since(last));
    entry.last_arrival = Some(now);

    // State-machine transition.
    let is_idle_chunk = is_turn_boundary
        && prev_gap.map(|g| g >= self.policy.idle_gap_threshold).unwrap_or(false);

    if is_idle_chunk {
        // Turn-boundary idle: promote to Interactive, reset counter.
        entry.state = CadenceState::Interactive;
        entry.continuous_arrival_count = 0;
    } else if is_turn_boundary {
        // Fast turn boundary (role:user but gap < threshold):
        // the user took over, so the continuous agentic run is broken,
        // but without an idle chunk there's no promotion.
        entry.continuous_arrival_count = 0;
        // State unchanged.
    } else {
        // Continuous arrival (role:tool / role:assistant).
        entry.continuous_arrival_count += 1;
        let count = entry.continuous_arrival_count;
        match entry.state {
            CadenceState::Cold | CadenceState::Interactive => {
                if count >= self.policy.agentic_suspected_threshold {
                    entry.state = CadenceState::AgenticSuspected;
                }
            }
            CadenceState::AgenticSuspected => {
                if count >= self.policy.agentic_confirmed_threshold {
                    entry.state = CadenceState::AgenticConfirmed;
                }
            }
            CadenceState::AgenticConfirmed => {
                // Already at the floor; stay.
            }
        }
    }

    prev_gap
}
```

### 4. Rewrite `classify_and_apply`

Signature stays the same; internals change:

```rust
pub fn classify_and_apply(&self, flow: &Flow, flow_id: &FlowId) {
    // Honor explicit overrides (header = 1, admin = 2).
    if flow.priority_source() != 0 {
        return;
    }
    if !self.policy.enabled {
        return;
    }

    let new_priority = {
        let entry = self.inner.entry(flow_id.clone()).or_default();
        entry.state.priority(&self.classes)
    };
    // DashMap guard dropped here.

    flow.set_priority(new_priority);
}
```

This is dramatically simpler than the old hysteresis-laced version — the
state machine *is* the hysteresis. Demotion goes through `AgenticSuspected`
before `AgenticConfirmed`; promotion is immediate on any idle chunk.

### 5. Remove dead code

Delete: `median_gap`, `classify`, `last_k_gap_all_le`, the old
`record_arrival(now, window)`, and the `Default` impl for `Cadence` if it
delegates to `new()` (keep it if it does — `or_default()` relies on it).

### 6. Update module doc comment

Replace the top-of-file doc that describes the median-gap model with the
state-machine model. Reference this plan (`docs/plans/006-turn-boundary-priority/`).

## State transition table (for reference)

| Current state | Event | New state | Counter |
|---|---|---|---|
| any | idle chunk (turn boundary + gap ≥ threshold) | `Interactive` | 0 |
| any | fast turn boundary (turn boundary + gap < threshold) | unchanged | 0 |
| `Cold` | continuous arrival | `Cold` or `AgenticSuspected` (if count ≥ 5) | +1 |
| `Interactive` | continuous arrival | `Interactive` or `AgenticSuspected` (if count ≥ 5) | +1 |
| `AgenticSuspected` | continuous arrival | `AgenticSuspected` or `AgenticConfirmed` (if count ≥ 12) | +1 |
| `AgenticConfirmed` | continuous arrival | `AgenticConfirmed` | +1 |

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
```

Full test pass will fail here — `tests/priority_heuristic.rs` still references
the old `record_arrival(flow_id, instant)` signature (no `is_turn_boundary`
param). That's expected; task 05 rewrites those tests. The build may also
fail in `src/scheduler/mod.rs:264` which calls the old signature — task 04
fixes that. If you want a clean build at this checkpoint, temporarily stub
`record_arrival` to accept the old signature and ignore the param. Otherwise
proceed to 04 which updates the call site.

To verify the cadence module in isolation, add a quick inline test in
`cadence.rs` covering the `[1,1,1,45,1,1,1,60]` pattern (idle chunks at 45s
and 60s → `Interactive`) and the continuous `[1,1,1,1,1,1,...]` pattern
(12 arrivals → `AgenticConfirmed`), then `cargo test --lib cadence`.
