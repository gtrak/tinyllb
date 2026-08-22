# 03 — Docs (lat.md)

## Objective

Update the knowledge graph so `gateway#Session Slot Pinning` and
`flow#Flow Registry and State` describe stateful allocation.

## Files

| File | Change |
|------|--------|
| `lat.md/gateway.md` | Session Slot Pinning: lead paragraph, Purpose, Non-goals (free-slot allocation is now in scope; observation stays out), Interface (`FlowRegistry::assign_slot` + `SlotAllocator`), Invariants (distinct-slot-while-capacity, hash-home common case, release on reap), Constraints (in-memory state, restart re-derivation), Rationale (why stateful; why hash home; why saturation fallback), Related (`src/flow/slots.rs#SlotAllocator`). |
| `lat.md/flow.md` | Flow Registry and State: Interface (slot assignment surface), Invariants (release on eviction), Related (`src/flow/slots.rs#SlotAllocator`). |

## Verification

- `lat check` passes (all wiki links + code refs resolve).
