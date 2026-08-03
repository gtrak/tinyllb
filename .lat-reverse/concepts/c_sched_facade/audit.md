# Audit: Scheduler Facade

**Scope:** `.lat-reverse/concepts/c_sched_facade/spec.md` vs `src/scheduler/mod.rs` (plus `kv_admission.rs`, `backpressure.rs`, `lifecycle.rs`, `fifo.rs`, `wfq.rs`, `drr.rs`, `flow_progress.rs` as referenced).
**Cycle:** 3 — Final auditor (contradictions only).

---

## "No How" Lint

**Result: PASS**

The spec uses domain concepts throughout:
- "admission" / "admit" — domain operation, not function name
- "queue depth" / "snapshot" — domain metrics, not field names
- "service total" / "credit" — domain quantities, not variable names
- "KV gate" — domain concept, not type name
- "flow progress tracker" — domain concept

No control flow descriptions, data structure details, function/method names as concept identifiers, or implementation-specific terminology detected in the spec body. The Interface section describes contractual surfaces using domain language. The Invariants section states universal properties. The Constraints section states design boundaries.

---

## Contradiction Report

### BUG — Code violates spec

**None found.** All spec claims verified against implementation.

Verification matrix:

| Spec Claim | Implementation | Status |
|---|---|---|
| KV gate runs before flow scheduler | `kv_policy.check().await?` before `match &self.inner` in `admit()` | Verified |
| Queue depth = inner depth + delayed count | `inner_depth + self.kv_policy.delayed_count()` | Verified |
| Snapshot waiting = inner waiting + delayed | `waiting: inner_snapshot.waiting + delayed as u64` | Verified |
| Algorithm-exhaustive dispatch (all match arms cover Fifo/Wfq/Drr) | All 5 delegating methods have 3-arm matches over `SchedulerImpl` | Verified |
| Non-work-tracking `service_done` returns zero | Fifo→0.0, Drr→0.0 (only Wfq returns real) | Verified |
| Non-credit `credit` returns zero | Fifo→0, Wfq→0 (only Drr returns real) | Verified |
| Non-accounting `report_accounting` is no-op | Fifo→{}, Wfq→{} (only Drr processes) | Verified |
| Full constructor has no failure mode | `pub fn new(...) -> Self` (infallible) | Verified |
| Defaults constructor: starvation=300s, completion_bias=Default, monitor=empty, kv_config=Default | `Duration::from_secs(300)`, `CompletionBias::default()`, `BackendMonitor::empty()`, `KvPolicyConfig::default()` | Verified |
| `KvPolicyConfig::default()` yields `enabled: false` | `config/mod.rs` confirms `enabled: false` in Default impl | Verified |
| `CompletionBias::default()` yields `enabled: true, target=0, predictive_admit: false` | `config/mod.rs` confirms | Verified |
| `BackpressureRejected` carries retry-after | `pub struct BackpressureRejected { pub retry_after: Duration }` | Verified |
| All listed re-exports present | 11 symbols confirmed in `pub use` lines | Verified |
| `pub mod lifecycle` exposed | `pub mod lifecycle` at line 7 | Verified |
| `flow_progress_tracker()` returns Arc | Returns `Arc<FlowProgressTracker>` (cloned) | Verified |
| Single algorithm per instance (immutable) | `inner: SchedulerImpl` set once in `new()` | Verified |
| Admission accepts flow_id + work_unit | `admit(&self, flow_id: FlowId, work_unit: f64) -> Result<QueueTicket, BackpressureRejected>` | Verified |

### SPEC_ERROR — Spec claims not implemented

**None found.** Every spec claim has a corresponding implementation in the code.

### UNDOCUMENTED_BEHAVIOR — Implementation behavior not described by spec

1. **Tracing instrumentation on `admit`** — The `admit` method carries `#[tracing::instrument(skip(self, flow_id, work_unit))]`, records `queue_depth_before` as a span field, and emits structured `info` events for accept/reject decisions with `wait_seconds`. The spec describes the admission contract but does not mention observability events, span fields, or trace recording.

2. **`algorithm_label` field** — The `Scheduler` struct stores `algorithm_label: &'static str` (derived from the configured algorithm variant) solely for tracing context. No spec section references a human-readable algorithm label or tracing context.

3. **`Policies` struct (`pub(crate)`)** — Internal shared-policy container holding completion bias gate, starvation timeout, notify, and flow progress tracker. Not exposed publicly and not described in the spec. **Appropriate omission** (internal implementation detail).

4. **`SchedulerImpl` enum** — Internal dispatch enum wrapping `FifoScheduler`, `WfqScheduler`, `DrrScheduler`. Not described in spec. **Appropriate omission** (internal implementation detail).

5. **`lifecycle::event` submodule constants** — The `lifecycle` module publicly exposes `pub mod event` with four string constants: `REQUEST_STARTED`, `TOKEN_RECEIVED`, `REQUEST_COMPLETED`, `REQUEST_CANCELLED`. The spec states the lifecycle submodule is publicly accessible but does not describe event name constants. **Contractually relevant** — callers depend on these exact strings for metrics correlation.

6. **`LifecycleGuard` type and methods** — The `lifecycle` module exposes `pub struct LifecycleGuard` with methods `new`, `record_token`, `add_delivered_tokens`, `mark_completed`, and Drop-based accounting. The spec says the submodule exposes "request lifecycle types" but does not describe `LifecycleGuard` or its contract surface. **Contractually relevant** — `LifecycleGuard` is the primary lifecycle handle that callers construct and drop to bridge request streaming with scheduler accounting.

7. **`FlowProgressTracker` public methods** — The re-exported `FlowProgressTracker` exposes `register`, `update_delivered`, `unregister`, `is_near_done`, and `any_flow_near_done`. The spec describes "flow progress tracker" as shared state accessible via the facade's `flow_progress_tracker()` method, but does not describe the tracker's own public API. **Contractually relevant** — callers with the Arc reference can invoke these methods directly.

### MISSING_INTERFACE — Spec claims interface not exposed by code

**None found.** All spec-described interfaces are present in the implementation.

---

## Verdict

The spec and implementation are aligned. No bugs, no spec errors, no missing interfaces.

Items 1-2 (tracing, algorithm_label) are observability-only with no effect on the public contract surface.

Items 3-4 (Policies, SchedulerImpl) are `pub(crate)` internal structures correctly omitted per the Interface-first principle.

Items 5-7 (`event` constants, `LifecycleGuard`, `FlowProgressTracker` methods) represent **contractually relevant** public surfaces in the `lifecycle` submodule and re-exported types that the spec alludes to without describing. These do not contradict the spec but are underspecified — the spec correctly identifies that these surfaces exist but omits their contract details. In a future integration cycle, these could be expanded or spun into separate concepts (`c_lifecycle`, `c_flow_progress`) with their own specs.
