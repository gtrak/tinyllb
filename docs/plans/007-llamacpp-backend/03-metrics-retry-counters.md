# 03 — Metrics: backend retry counters

- **Complexity:** XS
- **Timebox:** 15 min
- **Depends on:** nothing

## Objective

Register two Prometheus counters for the transient re-forward feature so
tasks 04/05 have call sites to increment. Follows the existing
`tinyllb_premature_stop_*` precedent.

## Files

| File | Change |
|------|--------|
| `src/metrics/mod.rs` | Add `backend_retries_total: Counter` and `backend_retry_exhausted_total: Counter` to the `Metrics` struct, register them, and document them in the module-level comment. |

## Context

- The `Metrics` struct and registry live in `src/metrics/mod.rs`. Existing
  counter precedent: `premature_stop_retries_total`, `premature_stop_exhausted_total`
  and `backend_stall_events_total`. Mirror their construction/registration
  exactly (the `prometheus::Counter::new(...).expect(...)`,
  `.register(Box::new(...)).expect(...)` pattern).
- The module-level comment block (`src/metrics/backend.rs` is just a comment
  file; the real struct is in `src/metrics/mod.rs`) lists metric families —
  add a one-line note for the new family.

## Steps

1. Add two fields to `Metrics`:
   ```rust
   pub backend_retries_total: prometheus::Counter,
   pub backend_retry_exhausted_total: prometheus::Counter,
   ```
2. In `Metrics::new` (or the zero-arg constructor), construct them:
   ```rust
   let backend_retries_total = prometheus::Counter::new(
       "tinyllb_backend_retries_total",
       "Proxy-side re-forwards of transient backend errors (llama.cpp context-exceed where prompt fits slot capacity, or mid-stream KV exhaustion before any content forwarded)",
   ).expect("tinyllb_backend_retries_total should be creatable");
   let backend_retry_exhausted_total = prometheus::Counter::new(
       "tinyllb_backend_retry_exhausted_total",
       "Transient backend retries exhausted (last error response forwarded to client)",
   ).expect("tinyllb_backend_retry_exhausted_total should be creatable");
   ```
3. Register both (`.register(Box::new(...)).expect("... registration should succeed")`).
4. Add them to the returned `Metrics { ... }` struct literal.
5. Add a short note to the `src/metrics/backend.rs` comment block
   (it documents the backend metric family) mentioning the two new
   counters — one or two lines, matching the existing comment style.

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all
```
The counters just need to exist and register without panic; wiring into the
gateway happens in tasks 04/05. A trivial test that `Metrics::new()` doesn't
panic and the counters are readable (`get() == 0`) is optional but welcome.
