# Concept: FlowRegistry — Extraction

Source: `src/flow/mod.rs`

## Responsibilities (observable behavior)

- Holds a thread-safe, concurrent collection of flows keyed by a flow identifier, each flow retained as a shared, clonable handle accessible to multiple consumers (`FlowRegistry` struct + `Arc<Flow>` handoff, mod.rs:149-158, 174-182). Evidence: `flows: DashMap<FlowId, Arc<Flow>>` (mod.rs:155), `pub fn get_or_create(&self, id: FlowId) -> Arc<Flow>` (mod.rs:174).
- Provides flow registration that both creates new flows and updates weight/priority of existing flows (upsert), reporting whether a creation occurred (`register`, mod.rs:188-211, return `bool`).
- Tracks per-flow scheduling-affecting attributes: weight (fair scheduling), priority (urgency), depth (queued request count), credit (deficit round-robin runtime), enqueued timestamp (starvation detection), and active in-flight request count (`Flow` struct, mod.rs:55-72).
- Produces queue-state snapshots for observability: a global active/waiting count plus an ordered list of currently-waiting flow entries (`QueueSnapshot`, mod.rs:236-266).
- Aggregates per-flow depth counters across the whole registry (`sum_depths`, mod.rs:224-229).
- Exposes a count query and emptiness check over registered flows (`len`, `is_empty`, mod.rs:214-221).
- Represents flow identifiers as an opaque dedicated type distinct from a raw string, with a defined ephemeral-vs-named classification and a metric label derivation (`FlowId`, mod.rs:14-39).

## Interface surfaces

### FlowRegistry — registration & lookup
- `get_or_create(id: FlowId) -> Arc<Flow>` — accepts a flow id. Returns a shared handle to an existing flow if present, otherwise creates one with the registry's default weight/priority and returns it. Produces a registered flow exactly once even under concurrent first-time requests (atomic check-and-insert, mod.rs:174-182). No error path; creates on absence.
- `register(reg: FlowRegistration) -> bool` — accepts an id/weight/priority tuple. If a flow with that id exists, updates its weight and priority in place; otherwise inserts a new flow with the supplied weight/priority. Returns `true` on insertion, `false` on update (mod.rs:188-211). No error path.
- `len() -> usize`, `is_empty() -> bool` — report number of registered flows / whether zero (mod.rs:214-221). No error path.

### FlowRegistry — weight/priority aggregation
- `sum_depths() -> u32` — sums the per-flow depth counters of all registered flows; overflow-wrapped at u32 (mod.rs:224-229).

### FlowRegistry — snapshot reporting
- `queue_snapshot(active: u64, waiting: u64, wait_order: I) -> QueueSnapshot` — accepts caller-supplied active and waiting totals and an iterator of flow ids denoting queue order. Produces a snapshot whose flows list includes only registered flows with depth > 0, deduplicated, each assigned a 1-based position reflecting its rank in the given ordering; unknown or non-waiting ids are skipped (mod.rs:231-259). No error path.

### Flow — per-flow attribute surface
- `new(id: FlowId, weight: f64, priority: u32) -> Flow` — creates a flow with explicit weight/priority and zero-initialized counters/timestamps (mod.rs:76-86).
- `weight() -> f64` / `set_weight(f64)` — read/write the weight; `priority() -> u32` / `set_priority(u32)` — read/write the priority (mod.rs:89-106). No error paths.
- `is_active() -> bool`, `inc_active()`, `dec_active()` — query and mutate the active in-flight count; a flow is non-active exactly when this count is zero (mod.rs:109-121). No error paths.

### FlowId — identifier semantics
- `FlowId::new(Into<String>)` — construct an identifier from a string (mod.rs:19). Opaque; not interchangeable with a raw string when typed as a flow id (mod.rs:10-13). Equality and hash are by underlying string (derive, mod.rs:14).
- `is_ephemeral() -> bool` — true exactly when the id string has a fixed prefix; only such ids are ephemeral (mod.rs:23-26).
- `metric_label() -> &str` — returns the id string, except for ephemeral ids which all return a single fixed label (mod.rs:28-38).

### Configuration contract
- `FlowRegistry::new(default_weight: f64, default_priority: u32)` — establishes the defaults applied to flows created via `get_or_create`; these defaults are not applied to flows created via `register` (which uses explicit values) (mod.rs:162-167 vs 188-211).

## Invariants

- A flow id maps to at most one registered flow; `get_or_create` and `register` never create two entries for the same id.
- Any flow returned by `get_or_create` is already present in the registry (its lookup is guaranteed to succeed thereafter). Evidence: entry API or_insert, mod.rs:178-181.
- A flow's priority/weight and counters are individually atomic, so concurrent updates do not tear a single attribute; cross-field coherence (e.g. weight and priority read together) is not guaranteed. Evidence: per-field atomics, mod.rs:61-71; separately stored write in `register` mod.rs:195-196.
- The snapshot `flows` list is deduplicated and ordered non-decreasing by the caller's supplied queue order; positions are exactly 1..n for the n listed waiting flows. Evidence: `seen` set + incrementing position, mod.rs:245-256.
- A flow with zero depth is never listed in `queue_snapshot` output. Evidence: depth>0 gate, mod.rs:250-257.
- For non-ephemeral ids, the metric label equals the id string. Evidence: mod.rs:31-38.

## Failure modes

- Priority and weight are read and written independently; a concurrent `register` and scheduler read can observe a mixed old/new pair (weight new, priority old, or vice versa), producing briefly inconsistent scheduling inputs. Evidence: two separate stores, mod.rs:195-200.
- `set_weight`/`set_priority` on a shared `Flow` handle do not coordinate with other writers, so last-writer-wins with no read-signaling. Evidence: Relaxed ordering, mod.rs:94-96, 104-105.
- `dec_active` can be called without a matching `inc_active`, underflowing the active count and causing `is_active` to report false while requests are genuinely in flight. Evidence: subtraction without clamping, mod.rs:119-121.
- An anonymous snapshot position numbering can mislead callers since skipped unknown/non-waiting ids cause renumbering; position is a relative order, not an absolute queue index. Evidence: position increments only for included flows, mod.rs:242-256.
- If the caller's `wait_order` omits a flow or omits a waiting flow, that flow is absent from the snapshot's flow list regardless of its depth; and a single snapshot reflects one point in time — the caller-supplied `active`/`waiting` counts may disagree with the computed flow sum. Evidence: snapshot list constrained to `wait_order`, mod.rs:244-246.
- `sum_depths` can overflow u32 and wrap silently if total queued counts across flows exceed the representable range. Evidence: u32 accumulation, mod.rs:224-227.