> Status: Historical plan snippet. The SchedulerImpl::Fifo/Wfq/Drr multi-algorithm dispatch shown below was collapsed to a single DrrScheduler (commit a3d9eee); the algorithm span field is now hardcoded "drr". Read the code snippets as the planned design, not current code.

# 04 — Scheduler Admit Signature

**Parent:** `PLAN.md`  
**Depends on:** `02-cadence-rewrite.md`, `03-turn-boundary-detection.md`

## Objective

Thread the `is_turn_boundary` signal from the proxy handler into the cadence
registry's `record_arrival` call inside `Scheduler::admit`. To avoid touching
~130 test/bench call sites that use `admit()`, add a new
`admit_with_turn_boundary()` method; the existing `admit()` becomes a thin
wrapper defaulting `is_turn_boundary = true` (optimistic).

## Files

| File | Change |
|---|---|
| `src/scheduler/mod.rs` | Add `admit_with_turn_boundary()`; make `admit()` delegate; pass `is_turn_boundary` to `cadence.record_arrival` |

## Steps

### 1. Add `admit_with_turn_boundary`

In `src/scheduler/mod.rs`, refactor the `admit` method (line ~254). Extract
the full body into `admit_with_turn_boundary`, and make `admit` delegate:

```rust
/// Attempt to admit a request into the active set.
///
/// Backward-compatible wrapper: defaults `is_turn_boundary = true`
/// (optimistic). All existing test/bench callers use this. The proxy
/// handler uses `admit_with_turn_boundary` to pass the detected value.
#[tracing::instrument(skip(self, flow_id, work_unit), fields(
    flow_id = %flow_id,
    queue_depth_before,
    algorithm = self.algorithm_label,
))]
pub async fn admit(
    &self,
    flow_id: crate::flow::FlowId,
    work_unit: f64,
) -> Result<QueueTicket, BackpressureRejected> {
    self.admit_with_turn_boundary(flow_id, work_unit, true).await
}

/// Admit with an explicit turn-boundary flag.
///
/// `is_turn_boundary = true` means the current request's last message has
/// `role: "user"` (or non-chat / optimistic). `false` means `role: "tool"`
/// or `"assistant"` (intra-turn continuation). The cadence state machine
/// uses this to distinguish turn-boundary idles from tool-execution gaps.
#[tracing::instrument(skip(self, flow_id, work_unit), fields(
    flow_id = %flow_id,
    queue_depth_before,
    algorithm = self.algorithm_label,
    is_turn_boundary,
))]
pub async fn admit_with_turn_boundary(
    &self,
    flow_id: crate::flow::FlowId,
    work_unit: f64,
    is_turn_boundary: bool,
) -> Result<QueueTicket, BackpressureRejected> {
    tracing::Span::current().record("queue_depth_before", self.queue_depth());

    // ── Priority cadence heuristic ──
    let flow = self.registry.get_or_create(flow_id.clone());
    let gap = self.cadence.record_arrival(
        &flow_id,
        std::time::Instant::now(),
        is_turn_boundary,
    );
    self.cadence.classify_and_apply(&flow, &flow_id);
    tracing::Span::current().record("priority", flow.priority());
    tracing::Span::current().record("priority_source", flow.priority_source());

    // ── Priority metrics ──
    self.metrics
        .flow_priority_class
        .with_label_values(&[flow_id.metric_label()])
        .set(flow.priority() as f64);
    if let Some(gap) = gap {
        self.metrics
            .flow_inter_request_seconds
            .with_label_values(&[flow_id.metric_label()])
            .observe(gap.as_secs_f64());
    }

    let enter = std::time::Instant::now();

    // KV policy gate runs FIRST before any flow scheduling.
    self.kv_policy.check().await?;
    let result = match &self.inner {
        SchedulerImpl::Fifo(s) => s.admit(flow_id, work_unit).await,
        SchedulerImpl::Wfq(s) => s.admit(flow_id, work_unit).await,
        SchedulerImpl::Drr(s) => s.admit(flow_id, work_unit).await,
    };

    let wait_secs = enter.elapsed().as_secs_f64();
    match &result {
        Ok(_) => {
            tracing::info!(decision = "accept", wait_seconds = wait_secs, "admit decision");
        }
        Err(_) => {
            tracing::info!(decision = "reject", wait_seconds = wait_secs, "admit decision");
        }
    }

    result
}
```

### 2. Remove the old `admit` body

The old `admit` method body (lines 254-311) is replaced by the delegation
above. Ensure the `#[tracing::instrument]` attribute moves to
`admit_with_turn_boundary` (or is duplicated on both — the wrapper doesn't
need its own span since it just delegates, but having it makes the span
appear for `admit()` callers too; keep it on both for clean spans).

### 3. Verify no other callers need updating

The only production caller of `admit` is `proxy.rs:636`, which task 03
updates to call `admit_with_turn_boundary`. All test/bench callers use
`admit()` with the optimistic default — no changes needed.

Confirm with:

```bash
rg '\.admit\(' --type rust src/ | grep -v 'admit_with_turn_boundary'
# Should only show the definition and the wrapper.
```

### 4. Span field for `is_turn_boundary`

The `admit_with_turn_boundary` span now records `is_turn_boundary` as a
field. This makes it visible in structured logs:

```
admit{flow_id=ses_... queue_depth_before=0 algorithm="drr" is_turn_boundary=true}: ...
```

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
```

At this point the build should succeed for `src/` (the cadence module, proxy,
and scheduler are all consistent). Test files that directly call
`record_arrival` with the old 2-arg signature will fail — those are rewritten
in task 05. Tests that only call `admit()` (the wrapper) should still compile
and pass.

```bash
# Verify the scheduler itself compiles:
cargo build -p tinyllb --lib
```
