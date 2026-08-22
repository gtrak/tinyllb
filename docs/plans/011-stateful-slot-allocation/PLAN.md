# Plan 011 — Stateful id_slot Allocation (collision-free)

Supersedes the stateless `fnv1a(flow) % n` slot mapping from plan 009.

## Why

Plan 009's assignment is a pure hash of the flow id modulo the slot count.
With a small `--parallel` (say 2 slots) two *live* sessions hash onto the same
slot with probability ~1/2. llama.cpp's slot holds **one** prompt KV cache, so
two pinned sessions on one slot evict each other's cache every turn — the
exact failure pinning exists to prevent. At n=2, 2 flows collide in half of
all deployments; the feature is unreliable in its most common small-server
configuration.

## What

Replace the per-request hash with a **stateful flow→slot allocator** owned by
the `FlowRegistry`:

- A new flow takes its deterministic hash **home slot** when free — so the
  common case (no collision) is byte-for-byte the plan 009 mapping, and a
  proxy restart re-derives the same mapping (warm server KV caches stay
  usable).
- If the home slot is already taken by another live flow, the new flow takes
  the **lowest free slot** instead. Two live flows never share a slot while
  the backend has spare capacity.
- If no slot is free (more live flows than slots — saturation), the new flow
  falls back to its hash home slot and shares it: the plan 009 behavior under
  load, which is unavoidable (n flows, m < n slots ⇒ someone shares).
- The allocator releases a flow's slot when the flow is reaped
  (`FlowRegistry::reap_idle`), freeing capacity for later flows.
- A defensive sweep in `reap_idle` drops assignments for flows no longer in
  the registry (leak guard; should not occur).

**Unchanged:** the pinning gate (named + inference only), the live
`slot_count` from the `/slots` snapshot (plan 010), `id_slot` integer
injection, retry-carry, and vLLM behavior (`slot_count: None` ⇒ no pinning,
byte-identical bodies).

## Design decisions

| Question | Answer | Why |
|----------|--------|-----|
| Where does state live? | `SlotAllocator` inside `FlowRegistry` | Slot lifetime = flow registry entry lifetime; reaping then frees slots with no new wiring (registry is already in `AppState`, already reaped every 60s). |
| First choice of slot? | Hash home (`fnv1a(flow) % n`) | Keeps plan 009's determinism in the collision-free case; restart-stable; no operator-visible reshuffle. |
| Collision resolution? | Lowest free slot | Deterministic given arrival order; frees the home slot's KV for the flow that owns it. |
| Saturation (no free slot)? | Hash home (share) | Deterministic; matches pre-stateful behavior; LRU-stealing would thrash warm caches with no latency win. |
| Slot count shrinks? | Stale assignment (index ≥ n) re-resolves on next request | Self-heals; no background task needed. |
| Concurrency? | Single `std::sync::Mutex`, no I/O inside | Critical section is a couple of HashMap ops; contention is at most two flows racing for first assignment. |

## Success criteria

1. Two named sessions whose hash homes collide (mod n) are pinned to
   **distinct** slots; each stays on its slot across turns.
2. A session that was reaped and returns re-takes its freed home slot.
3. All plan 009/010 regression gates hold: named→pinned integer,
   ephemeral/disabled→absent + byte-identical, retry carry, vLLM untouched.
4. `cargo clippy --all-targets -- -D warnings`, `cargo build --all-targets`,
   `cargo test --all`, `lat check` all pass.

## Task breakdown

| # | Task | Files | Complexity |
|---|------|-------|-----------|
| 01 | `SlotAllocator` (assign/release/sweep + unit tests) | src/flow/slots.rs, src/flow/mod.rs | M |
| 02 | Registry + gateway wiring; integration tests | src/flow/mod.rs, src/gateway/proxy.rs, tests/slot_pinning.rs | M |
| 03 | Docs: lat.md (gateway Session Slot Pinning, flow registry) | lat.md/gateway.md, lat.md/flow.md | S |

Tasks are ordered 01 → 02 → 03.
