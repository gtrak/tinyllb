# c_gateway_stream — Audit

**Status:** Second-correction audit (fresh)
**Source:** `src/gateway/stream.rs`
**Spec:** `.lat-reverse/concepts/c_gateway_stream/spec.md`

---

## "No How" Lint

The spec passes the No How constraint. It uses domain-level concept identifiers ("Active-request guard", "Passthrough stream", "Instrumented stream") rather than function or struct names in Purpose, Interface, Invariants, Constraints, and Rationale sections. Source code wiki links appear exclusively in the Related section, consistent with reconstruction.md scope restriction. No control flow descriptions, data structure details, or implementation-specific terminology were found outside the Related section.

---

## Findings

### 1. `MetricStream` omits error-level log on backend errors — **undocumented_behavior**

**Spec claim (Interface → Passthrough stream):** "Emits an error-level log entry whenever a backend error occurs before terminating; the log records the backend error for operational observability."

**Implementation:** `PassthroughStream::poll_next` emits `tracing::error!(...)` on backend errors. `MetricStream::poll_next` does NOT emit any log on backend errors — it wraps and returns the error silently.

**Analysis:** The spec lists error logging under Passthrough stream's interface surface. The Instrumented stream section states the variant is "indistinguishable at the interface from the passthrough variant," but this phrasing refers to the `Stream<Item = Result<Bytes, std::io::Error>>` type contract, not side effects. The spec does not explicitly require or prohibit error logging for the instrumented variant. The implementation omits it, and the spec is silent. This is an undocumented behavioral gap between the two stream variants.

**Severity:** Low — error logging is an operational side effect, not a consumer-visible contract.

---

### 2. Token positivity check operates on aggregate, not per value — **bug**

**Spec claim (Interface → Instrumented stream):** "Increments a token-generation counter for each parseable `completion_tokens` value greater than zero in the payload; parsing failures and non-positive values are silently skipped."

**Implementation:** `TokenAccumulator::extract_tokens` sums ALL parsed values into `found_tokens` (regardless of individual sign), then returns the aggregate. `MetricStream::poll_next` checks `if tokens > 0` on the aggregate result.

**Discrepancy:** The spec uses "each parseable `completion_tokens` value greater than zero" — the word "each" indicates per-value semantics: individual non-positive values should be filtered out before aggregation. The implementation sums first, then applies the positivity check to the net total. If a single chunk contains `completion_tokens: 5` and `completion_tokens: -2`, the spec requires counting only 5; the implementation counts 3.

**Severity:** Low — negative `completion_tokens` values are pathological and unlikely in practice. The behavioral divergence only manifests when multiple values coexist in a single chunk with mixed sign.

---

### 3. "Accumulated parse state persists across error boundaries" is ambiguous — **spec_error**

**Spec claim (Constraints):** "Token parsing can extract values whose JSON representation spans multiple byte chunks; accumulated parse state persists across error boundaries."

**Analysis:** "Error boundaries" is underspecified. Two readings exist:
- **Chunk boundaries** (between successive successful polls): The `TokenAccumulator.buffer` field persists across calls to `feed()`, so this reading is true.
- **Error event boundaries** (across `Some(Err(...))` returns): If a backend error occurs, the stream typically terminates and the consumer drops the `MetricStream`, losing the `TokenAccumulator.buffer`. This reading is false.

The first reading is the practical intent. The second reading is what "error boundaries" literally suggests. The spec should clarify "across chunk boundaries" to eliminate ambiguity.

**Severity:** Low — the practical intent is clear from context; the literal reading is implausible given stream semantics.

---

### 4. "Each time the stream yields None" is imprecise — **spec_error**

**Spec claim (Invariants):** "A completion signal is emitted to the lifecycle guard each time the stream yields `None` (exhaustion); it is not emitted on error or timeout paths."

**Analysis:** Rust's `Stream` trait contract guarantees that once `poll_next` returns `Poll::Ready(None)`, the consumer must stop polling. A conforming consumer receives `None` exactly once. The phrasing "each time" suggests the stream could yield `None` multiple times, which contradicts the `Stream` trait contract. The implementation calls `lifecycle_guard.mark_completed()` exactly once (in the `None` branch), which is correct — the spec wording is merely imprecise.

**Severity:** Negligible — no behavioral impact; purely linguistic.

---

## Verification Matrix

| Spec Claim | Implementation | Verdict |
|---|---|---|
| Guard increments on construction, decrements on drop | `new()` → `inc()`, `Drop::drop` → `dec()` | ✅ |
| Bytes forwarded verbatim (both variants) | Both yield `chunk` unmodified | ✅ |
| Backend errors wrapped as `std::io::Error::other` | `std::io::Error::other(err.to_string())` | ✅ |
| Passthrough emits error-level log on backend error | `tracing::error!(...)` present | ✅ |
| Token counter increments only for positive values | `if tokens > 0 { inc_by(...) }` | ⚠️ Aggregate check, not per-value |
| Lifecycle guard receives token reports | `add_delivered_tokens()` + `record_token()` | ✅ |
| Queue ticket held for stream lifetime | `_queue_ticket` field dropped with stream | ✅ |
| Deadline check before each poll | `Instant::now() >= deadline` → `Err` | ✅ |
| Completion signal on `None` only (not error/timeout) | `mark_completed()` in `None` branch only | ✅ |
| Parse state accumulates across chunks | `TokenAccumulator.buffer` persists across `feed()` calls | ✅ |
| Error-level log on MetricStream backend errors | Absent — see Finding 1 | ⚠️ |

---

## Summary

| Category | Count |
|---|---|
| bug | 1 |
| spec_error | 2 |
| undocumented_behavior | 1 |
| missing_interface | 0 |

The spec and implementation are substantially aligned. The single bug (per-value vs. aggregate positivity check) has low practical impact. Both spec errors are linguistic imprecisions with no behavioral consequence. The undocumented behavior (error logging divergence between stream variants) reflects a genuine spec gap that should be clarified in the next correction cycle.
