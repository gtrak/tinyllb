# 06 — Metrics

**Parent:** `PLAN.md`  
**Depends on:** `04-scheduler-admit.md`

## Objective

Add a `llm_flow_cadence_state` gauge that disambiguates `Cold` (priority 100)
from `Interactive` (also priority 100). The existing
`llm_flow_priority_class` gauge only shows the numeric priority, so two
flows at 100 are indistinguishable. The state gauge makes the state machine
observable.

This task is optional — the priority gauge and inter-request histogram from
Plan 004 continue working unchanged. The state gauge is additive.

## Files

| File | Change |
|---|---|
| `src/metrics/mod.rs` | Add `flow_cadence_state: GaugeVec` field, constructor, registration |
| `src/scheduler/mod.rs` | Set the gauge in `admit_with_turn_boundary` after `classify_and_apply` |
| `src/flow/cadence.rs` | Expose `CadenceState` via a public accessor on `CadenceRegistry` |
| `tests/metrics.rs` | Add test that the new gauge is registered and set |

## Steps

### 1. `src/flow/cadence.rs` — expose state

Add a method on `CadenceRegistry` to read a flow's current state:

```rust
/// Returns the flow's current cadence state, or `Cold` if not yet tracked.
pub fn state_of(&self, flow_id: &FlowId) -> CadenceState {
    self.inner.entry(flow_id.clone()).or_default().state
}
```

`CadenceState` and its variants must be `pub` (add `#[derive(Clone, Copy,
Debug, PartialEq, Eq)]` if not already present in task 02).

### 2. `src/metrics/mod.rs` — add the gauge

In the `Metrics` struct (line ~82), add:

```rust
pub flow_cadence_state: GaugeVec,
```

In `create_metrics()` (line ~297), construct it:

```rust
let flow_cadence_state = GaugeVec::new(
   (Opts::new(
        "llm_flow_cadence_state",
        "Per-flow cadence state machine state (0=cold, 1=interactive, 2=agentic_suspected, 3=agentic_confirmed)",
    )),
    &["flow_id"],
)
.expect("llm_flow_cadence_state should be creatable");
```

Register it (near line ~447):

```rust
.register(Box::new(flow_cadence_state.clone()))
.expect("llm_flow_cadence_state registration should succeed");
```

Add to the struct construction (line ~508):

```rust
flow_cadence_state,
```

### 3. `src/scheduler/mod.rs` — set the gauge

In `admit_with_turn_boundary`, after `classify_and_apply`:

```rust
self.metrics
    .flow_cadence_state
    .with_label_values(&[flow_id.metric_label()])
    .set(self.cadence.state_of(&flow_id) as u32 as f64);
```

Map `CadenceState` to a numeric value via `as u32` — ensure the enum
discriminants are explicit:

```rust
#[repr(u32)]
pub enum CadenceState {
    Cold = 0,
    Interactive = 1,
    AgenticSuspected = 2,
    AgenticConfirmed = 3,
}
```

### 4. `tests/metrics.rs` — test

Add a test that asserts the gauge is registered and can be set:

```rust
metrics.flow_cadence_state.with_label_values(&["test"]).set(1.0);
// ... collect metric names, assert "llm_flow_cadence_state" is present
```

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo test --all --test metrics
curl -s http://localhost:1234/metrics | grep llm_flow_cadence_state
```

The `/metrics` output should show:

```
# HELP llm_flow_cadence_state Per-flow cadence state machine state (0=cold, ...)
# TYPE llm_flow_cadence_state gauge
llm_flow_cadence_state{flow_id="ses_..."} 1
```
