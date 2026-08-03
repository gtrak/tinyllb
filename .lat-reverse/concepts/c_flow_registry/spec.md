# Concept: FlowRegistry — Spec

## Purpose

The flow registry is the authoritative source of scheduling-entity state for all consumer subsystems. It guarantees that every flow identity maps to exactly one registered entry and provides concurrent access to independently usable flow references. Consumers depend on it for creation-time defaults, weight and priority upserts, aggregate depth queries, and point-in-time queue snapshots. The registry itself does not manage scheduling policy; it maintains the per-flow attributes that scheduling policy reads.

## Non-goals

The registry is not a queue and does not define ordering among waiting flows; ordering is supplied externally when snapshots are produced. It does not validate weight or priority ranges, enforce scheduling policy, or coordinate depth and credit counter updates with scheduling decisions. Flows cannot be removed; the registry is a monotonic collection. The active-count mechanism tracks in-flight presence but provides no underflow protection or mutual exclusion with other subsystems.

## Interface

The registry exposes contractual surfaces covering construction, registration, lookup, aggregate queries, snapshots, per-flow attributes, and flow identity. Each surface describes what callers depend on, independent of internal implementation.

### Construction

The registry is instantiated with a default weight and default priority. These defaults apply only to flows created through lookup; flows created through registration use the explicit values in the registration payload. A flow can also be constructed independently of any registry, with an identity, weight, and priority supplied at creation time.

### Registration Payload

`FlowRegistration` is a public data type with public fields: identity, weight, and priority. Callers construct instances directly and pass them to the registration operation.

### Registration

Registering a flow either creates a new entry or updates an existing one's weight and priority; the operation always succeeds and reports whether an insertion occurred. Registration uses the weight and priority from the payload rather than registry defaults.

### Lookup

Looking up a flow by identity returns an independently usable shared flow reference; if the flow is not yet registered, one is created with the registry's default weight and priority. Concurrent first-time lookups for the same identity yield a single registered entry.

### Aggregate Queries

The registry reports the count of registered flows and whether it is empty. It also computes the sum of all per-flow depth counters as an unsigned 32-bit integer. These queries read live state without modifying the registry; the depth sum may overflow on extreme depth totals with overflow semantics depending on the compilation profile.

### Queue Snapshots

`QueueSnapshot` is a public data type with public fields: a global active count, a global waiting count, and a list of `QueueFlowEntry` items. Each `QueueFlowEntry` has public fields: an identity string and a 1-based position. Producing a snapshot requires the caller to supply the global counts and an ordered list of identities. The registry filters to registered flows with positive depth, deduplicates entries, assigns contiguous positions preserving the caller's relative ordering, and discards unknown or zero-depth identities.

### Per-Flow Attributes

Each flow exposes weight (fair-scheduling factor), priority (urgency class), credit (deficit round-robin accumulator), depth (queued request count), enqueued timestamp (starvation detection point), and active in-flight request count. Weight and priority are individually readable and writable through dedicated methods. The remaining attributes — depth, credit, enqueued timestamp, and active count — are directly accessible as public fields with their own atomic or locked mutation surfaces; a consumer can mutate them without registry coordination. The active counter also supports increment and decrement operations. A flow is considered active when its in-flight count is nonzero.

### Flow Identity

A flow identity is an opaque dedicated type, not interchangeable with a raw string. It supports identity construction from a string, equality by underlying string value, and display that outputs the identity string. Debug output wraps the string with a type prefix. The identity classifies as ephemeral exactly when its string value begins with the `"ephemeral-"` prefix. For metric labeling, ephemeral identities resolve to a single common label; named identities resolve to their identity string.

## Invariants

All statements about the registry remain true regardless of implementation details.

- Each flow identity maps to at most one registered entry; creation paths never produce duplicates for the same identity.
- Once a flow is created through lookup or registration, it remains registered for the lifetime of the registry; identities cannot be unregistered.
- Weight, priority, credit, and depth are updated individually; no cross-attribute atomicity is guaranteed between any pair of attribute updates.
- Snapshot outputs list only flows with positive depth, contain no duplicates, and assign contiguous 1-based positions starting from one.
- The metric label for ephemeral identities always resolves to a single common value; for named identities, the metric label equals the identity string.

## Constraints

The registry operates under explicit limitations that shape its safe usage.

- Weight and priority updates are not mutually exclusive with concurrent reads; consumers may observe briefly inconsistent attribute pairs during a register operation.
- The active-count decrement uses unsaturated subtraction on an unsigned counter: in debug builds, calling decrement without a matching increment panics; in release builds, the value wraps to the maximum representable integer, which the active-check interprets as nonzero (active), not false.
- The aggregate depth sum may overflow the 32-bit range under extreme queued depth; overflow behavior depends on the compilation profile.
- Snapshot positions reflect relative order among included flows only; skipped identities renumber subsequent positions, so position is not an absolute queue index.
- Snapshot global counts (active, waiting) are caller-supplied and are not cross-checked against the computed per-flow data; discrepancies between global totals and per-flow sums indicate stale inputs, not registry errors.

## Rationale

A centralized registry separates identity management and attribute storage from scheduling logic, allowing the scheduling subsystem to read per-flow state without coordinating access. Concurrent access to flow references avoids serializing all consumers through a single synchronization point. The ephemeral-vs-named identity distinction enables coarse-grained metric aggregation for anonymous workloads while preserving fine-grained tracking for named flows. Defaults on lookup support dynamic flow discovery where the caller supplies only an identity, while explicit registration allows policy-level control over weight and priority. Direct field access on counter attributes enables subsystems that manage depth, credit, and active state to update them without indirection, accepting the trade-off that coordination remains the caller's responsibility.

## Related

[[?c_flow_id]] — Identity semantics and ephemeral classification
[[?c_scheduling_policy]] — Consumer that reads weight, priority, depth, and credit from registered flows
[[?c_queue_observability]] — Queue snapshot surface and its interpretation
[[src/flow/mod.rs#FlowRegistry]] — Registry implementation
[[src/flow/mod.rs#Flow]] — Per-flow attribute storage
[[src/flow/mod.rs#FlowId]] — Identity and label derivation
