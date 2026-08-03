# Audit: c_api_queue

**Scope**: `.lat-reverse/concepts/c_api_queue/spec.md` vs. `src/api/queue.rs`
**Status**: 1 finding

---

## Findings

### 1. missing_interface — HTTP method and path omitted from Interface section

**Spec claim**: The Interface section states "Accepts an unauthenticated request with no required parameters" but does not specify the HTTP method or the URL path.

**Code evidence**: `src/api/queue.rs` line 31 declares the handler for `GET /queue` (documented in the doc comment at line 27–28).

**Impact**: A consumer reading the spec cannot determine which endpoint to call. The reconstruction.md rules require HTTP interfaces to document "method + path". The Interface section omits both.

---

## No-How Lint

The spec contains no violations:
- No function or method names used as concept identifiers
- No control flow descriptions
- No data structure internals
- No implementation-specific terminology

All Interface, Invariants, Constraints, and Rationale statements are expressed in domain-contract language. The Related section correctly uses `[[src/api/queue.rs#QueueResponse]]` and `[[src/api/queue.rs#FlowPosition]]` source links, which is permitted per reconstruction.md rules.

---

## Verification Matrix

| Spec Claim | Implementation | Verdict |
|---|---|---|
| Three response fields: active count, waiting count, per-flow positions | `QueueResponse { active: u64, waiting: u64, flows: Vec<FlowPosition> }` | Matches |
| Each position entry has flow identifier + 1-indexed position | `FlowPosition { id: String, position: u64 }` | Matches |
| No authentication required | No auth extractor in handler signature | Matches |
| No error status codes; always same structural shape | Returns `Json<QueueResponse>` (infallible handler) | Matches |
| Active + waiting = total flows represented | Tautological by definition (active + waiting = active + waiting) | Matches |
| Position values form sequence 1, 2, 3, … | Delegated to scheduler; API layer passes through | No contradiction (depends on scheduler contract) |
| Active flows excluded from per-flow list | Comment confirms "only for flows currently queued"; passthrough from scheduler | No contradiction (depends on scheduler contract) |
| No queue mutation | Read-only: calls `state.scheduler.queue_snapshot()` only | Matches |
| No filtering/pagination/query params | No query extractors; returns full snapshot | Matches |
| Staleness not bounded | Single `queue_snapshot()` call | Matches |
