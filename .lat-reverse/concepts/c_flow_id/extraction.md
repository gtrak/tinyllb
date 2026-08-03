# Extraction: [[c_flow_id]] — FlowId

Source: `src/flow/mod.rs`

## Responsibilities

- Provides an opaque identity type that labels a logical client/workload whose requests are scheduled together (src/flow/mod.rs:10-15).
- Distinguishes two identity classes by prefix: ephemeral (auto-generated) vs. named (src/flow/mod.rs:24-26).
- Supports equality/comparison, hashing, display, debugging, and conversion to a Prometheus label form (src/flow/mod.rs:14, 32-38, 41-51).
- Serves as the registry key for flows (src/flow/mod.rs:149-155, 174, 188, 208).

## Interface surfaces

- **Construction (`FlowId::new`, src/flow/mod.rs:19-21)** — accepts any value convertible to a string; produces a FlowId. No validation: any string is accepted, including empty strings and strings with the `ephemeral-` prefix. No error path. Callers may construct an ID carrying the ephemeral marker even for logically named flows.
- **Ephemeral detection (`is_ephemeral`, src/flow/mod.rs:24-26)** — accepts any FlowId; returns `true` iff the underlying string starts with `ephemeral-`. Never errors. Property test: `FlowId::new("ephemeral-xyz").is_ephemeral() == true`; `FlowId::new("named").is_ephemeral() == false`.
- **Prometheus label (`metric_label`, src/flow/mod.rs:32-38)** — accepts any FlowId; returns `&str`. Postcondition: ephemeral IDs always map to the literal string `"ephemeral"` (aggregation to avoid cardinality explosion); named IDs return the underlying string unchanged. Never errors. Lifetime ties to the FlowId.
- **Equality/hash semantics (`PartialEq`, `Eq`, `Hash` derive, src/flow/mod.rs:14)** — two FlowIds are equal iff their underlying strings are equal; equality is reflexive, symmetric, transitive; `Hash` is consistent with equality. `Clone` yields an equal, independent value.
- **Display (`Display`, src/flow/mod.rs:47-51)** — accepts any FlowId; renders the underlying string verbatim. Used by `queue_snapshot` to stringify IDs (src/flow/mod.rs:245-253).
- **Debug (`Debug`, src/flow/mod.rs:41-45)** — accepts any FlowId; renders as `FlowId(<string>)`, wrapping the underlying string. Never errors.
- **Registry key role (src/flow/mod.rs:149-211)** — FlowId is the lookup key for the flow registry; distinct IDs never alias (distinct underlying strings are distinct entries). Note inconsistency: `register` reads the ID from the registration, while `get_or_create` keys by the passed ID; both rely on string equality semantics.

## Invariants

- The underlying string is immutable after construction (no mutation API; only `new` builds it) (src/flow/mod.rs:14-38). Identity is therefore stable for the lifetime of the value.
- `is_ephemeral()` and `metric_label()` are pure functions of the underlying string; for a given FlowId the results never change (src/flow/mod.rs:24-38).
- `metric_label` postcondition: `metric_label() == "ephemeral"` iff `is_ephemeral()` (src/flow/mod.rs:32-38).
- A FlowId created via `new` always `Display`s exactly to the string it was constructed from (src/flow/mod.rs:19-21, 47-51).
- Equality is exact string equality — no canonicalization, normalization, trimming, or case folding is applied (src/flow/mod.rs:14).

## Failure modes

- **Prefix collision** — a caller can construct a named FlowId beginning with `ephemeral-`, making a named flow indistinguishable from an ephemeral one; `is_ephemeral`/`metric_label` then misreport it as ephemeral and its metrics collapse into the aggregate (src/flow/mod.rs:19-21, 24-38).
- **Empty/whitespace IDs** — `new` accepts empty or whitespace-only strings with no validation; such IDs are accepted into the registry and can produce empty display labels and degenerate Prometheus label values (src/flow/mod.rs:19-21, 47-51).
- **Cardinality mis-attribution** — classification is purely prefix-based, so any string starting with `ephemeral-` (including a user-chosen one) is silently aggregated under one label; there is no independent marker proving auto-generation (src/flow/mod.rs:24-38).
- **Registry stringification dependence** — consumers that stringify FlowId for dedup/ordering (e.g. `queue_snapshot`, src/flow/mod.rs:245-253) depend on `Display` verbatim rendering; any future change to display formatting would silently change dedup keys.

## Related

- `src/flow/mod.rs` (Flow, FlowRegistry, FlowRegistration, QueueSnapshot — consumers of FlowId)
