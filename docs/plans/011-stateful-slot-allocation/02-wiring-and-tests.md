# 02 — Registry + Gateway Wiring, Integration Tests

## Objective

Route `id_slot` selection through the registry-owned allocator and prove
collision avoidance end-to-end.

## Files

| File | Change |
|------|--------|
| `src/flow/mod.rs` | `FlowRegistry` gains `slots: SlotAllocator`; new `assign_slot(&FlowId, u32) -> u32`; `reap_idle` releases reaped flows' slots + defensive `sweep_missing`; Debug shows slot count. |
| `src/gateway/proxy.rs` | `id_slot` match arm calls `state.flow_registry.assign_slot(&flow_id, n)`; drop the now-unused `slot_id_for_flow` import. |
| `tests/slot_pinning.rs` | Test 8: two sessions with colliding hash homes (mod 2) get distinct slots, stable across turns. Test 9: 3 sessions / 2 slots → third takes its hash home (saturation). `find_colliding_sessions(n)` helper (deterministic FNV search). |

## Steps

1. `FlowRegistry::new` initializes `slots: SlotAllocator::new()`.
2. `assign_slot` delegates to the allocator (proxied through the registry so
   slot lifetime == registry entry lifetime, with no new `AppState` field).
3. `reap_idle`: after removing each flow, `slots.release(id)`; then
   `slots.sweep_missing(|id| self.flows.contains_key(id))` (leak guard).
4. Proxy: replace `slot_id_for_flow(&flow_id.to_string(), n)` with
   `state.flow_registry.assign_slot(&flow_id, n)`.
5. Integration tests as above; the first session of a fresh app keeps its hash
   home (asserted), so the plan 009 mapping is pinned as the common case.

## Verification

- `cargo test --test slot_pinning` — all 9 pass (7 pre-existing + 2 new).
- Existing gates intact: `disabled_omits_id_slot` byte-identity,
  `id_slot_survives_retry`, `ephemeral_omits_id_slot`.
