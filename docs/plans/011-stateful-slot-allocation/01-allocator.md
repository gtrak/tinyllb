# 01 — SlotAllocator

## Objective

Add `src/flow/slots.rs` with a `SlotAllocator`: a concurrent, in-memory
flow→slot map implementing home-slot-first, lowest-free-slot collision
resolution, hash-home saturation fallback, stale-count reassignment, release,
and an orphan sweep.

## Files

| File | Change |
|------|--------|
| `src/flow/slots.rs` | New module: `SlotAllocator` (`new`, `assign`, `release`, `sweep_missing`, `len`, `is_empty`, `Default`) + unit tests. |
| `src/flow/mod.rs` | `pub mod slots;` + `pub use slots::SlotAllocator;`. |

## Steps

1. `struct Inner { assignments: HashMap<FlowId, u32>, occupied: HashMap<u32, u32> }`
   behind a `Mutex`; `occupied` counts per-slot so saturation (2+ flows on a
   slot) is representable.
2. `assign(flow, n)`:
   - existing assignment with `slot < n` → return it;
   - existing but `slot >= n` (count shrank) → drop, re-assign;
   - else home = `slot_id_for_flow(flow, n)`; free → home; else lowest free in
     `0..n`; else home (saturation).
   - record assignment + increment `occupied`.
3. `release(flow)`: remove assignment, decrement `occupied` (drop at 0).
4. `sweep_missing(exists)`: drop assignments whose flow fails `exists`;
   return count.
5. Unit tests: stability, colliding-home distinctness, release+retake,
   saturation fallback, shrunken-count reassignment, sweep.

## Verification

- `cargo test --lib flow::slots` passes.
- `debug_assert!(slot_count >= 1)` — callers gate on it.
