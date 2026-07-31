# 16 — Dynamic Admission + Token Feedback Loop

**Phase:** 3 (vLLM Integration)
**Depends on:** `15`.
**Blocks:** `17`.

## Objective

Close the loop opened by Phase 2's estimate-based credit accounting and
PRD §6.3's "Optional future integration."  Replace the `max_tokens` **estimate**
used for DRR cost (`11`) with the **actual** generated-token count reported by
the backend's streaming `usage` frame.  Add token-level feedback so completion
bias and DRR credit reflect reality, not the optimistic upper bound.

This is PRD §5 "Future: token-level feedback" within the in-plan scope
(we built the architecture for it in `11`/`13`).

## Files

| File | Change |
| --- | --- |
| `src/gateway/stream.rs` | Edit: parse every SSE chunk for `usage`; emit final count. |
| `src/scheduler/drr.rs` | Edit: consume actual `tokens_generated` not estimate; reconcile under/over. |
| `src/scheduler/completion_bias.rs` | Edit: track per-flow live `tokens_generated` for bias decisions. |
| `src/metrics/throughput.rs` | Edit: `llm_tokens_generated_total` increments from actual usage. |
| `tests/token_feedback.rs` | New: actual-vs-estimate accounting under various stream shapes. |

## Steps

1. SSE parser: accumulate `data: {...}` frames; capture the final
   `usage.completion_tokens` (and `prompt_tokens` if present).  Handle the
   non-streaming path too where `usage` is in the top-level response JSON.
2. DRR credit reconciliation: at request finish, the exact consumed cost =
   `actual_tokens_generated`.  If the request had costed `max_tokens` in
   credit (`11`) but only generated `X < max_tokens`, the
   unused `max_tokens - X` is **restored** to the flow credit (same rule as
   cancel-restoration in `13`, generalized).  If `X > max_tokens` (rare;
   backend ignores cap), debit the overrun so the next request's credit is
   fair.
3. Completion bias now uses live `tokens_generated` per active flow to
   estimate "is this flow near done?" — if a flow has produced within, say,
   10% of its `max_tokens`, completion bias can pre-admit the next flow
   earlier (predictive admit).  Keep this off by default behind a config
   flag `completion_bias.predictive_admit: false`.
4. `llm_tokens_generated_total` (from `04`) previously best-effort becomes
   authoritative; deprecate the "best effort parse" fallback path with a
   warning log so we surface when a backend doesn't emit `usage`.
5. Tests:
   * stream emits `usage.completion_tokens=512` after `max_tokens=8192` was
     costed; assert credit restored to `(credit - 512)`,
   * non-stream response mirrors the same accounting,
   * backend that emits **no** usage frame logs a warning and falls back to
     the estimate (no panic, scheduler keeps working),
   * predictive admit OFF by default; ON allows pre-admit per the threshold.

## Verification

* `cargo test --test token_feedback` green.
* `/metrics` `llm_tokens_generated_total` after a real request equals the
  backend's reported `usage.completion_tokens`.
* A flow that requests `max_tokens=8192` but completes in 512 tokens does not
  have its credit drained as if it produced 8192 — confirmed via
  `llm_flow_credit{flow_id}` snapshot before/after.
* Backend with no `usage` frame still serves via the estimate path; a warning
  is logged.
