# Spec: [[c_flow_id]] — FlowId

## Purpose

FlowId is an opaque identity token that labels a logical client or workload whose requests are scheduled together. This spec covers the FlowId type exclusively; request-time resolution and registry semantics belong to [[?c_flow_identify]] and [[?c_flow_registry]].

- Uniquely identifies a scheduling unit (a logical client or workload).
- Classifies identity into two mutually exclusive classes: ephemeral (auto-generated) and named (user-assigned).
- Maps each identity to a metric label, collapsing ephemeral identities to a single aggregate label to bound cardinality.
- Supports exact equality and hashing for use as a lookup key in associative collections.
- Renders the underlying string verbatim for observability surfaces.

## Non-goals

FlowId does not validate, normalize, or otherwise sanitize the strings it wraps; it delegates classification to a simple prefix convention and leaves correctness to callers.

- Does not validate input strings (empty, whitespace, or arbitrary content are accepted).
- Does not canonicalize, trim, or case-fold identifiers.
- Does not provide an independent marker distinguishing auto-generated ephemeral IDs from user-chosen ones that happen to share the same prefix.
- Request-time resolution of identities from HTTP headers or body metadata belongs to [[?c_flow_identify]], including precedence chains, empty-value rejection, and fallback generation.
- Does not track creation time, origin, or lifecycle state beyond the identity string itself.

## Interface

The interface provides construction, classification, metric-label derivation, equality semantics, and string rendering for FlowId values.

- **Direct construction** — accepts any string-like value; produces a stable identity. No validation or error path; any string, including empty or prefix-bearing values, is accepted.
- **Classification** — determines whether an identity belongs to the ephemeral class; the result is a pure function of the identity's underlying string and never changes.
- **Metric label derivation** — maps each identity to a stable label string. Ephemeral identities always produce the aggregate label `"ephemeral"`; named identities yield their own string. Empty-string identities produce an empty label.
- **Equality and hashing** — two identities are equal if and only if their underlying strings are equal; equality is reflexive, symmetric, and transitive. Hashing is consistent with equality. Cloning yields an independent but equal value.
- **Rendering** — Display renders the underlying string verbatim. Debug renders with the type-wrapped format `FlowId(<string>)`, where `<string>` is the identity value.

## Invariants

The identity value is immutable after construction; all derived properties are stable for its lifetime.

- The underlying string never changes after construction; there is no mutation API.
- Classification is a pure function of the underlying string; for a given identity the result never changes.
- An identity yields the aggregate metric label if and only if it is ephemeral.
- Display rendering reproduces the construction string exactly — no transformation, trimming, or encoding is applied.
- Equality is exact string equality — no canonicalization, normalization, or case folding occurs.

## Constraints

Classification and metric behavior are governed entirely by a string prefix convention; there is no independent state or attribute distinguishing identity classes.

- Ephemeral classification is determined solely by an `ephemeral-` string prefix; any string beginning with that prefix is treated as ephemeral, including user-chosen names. This is accepted behavior, not a defect.
- Auto-generated ephemeral IDs use the format `ephemeral-{UUIDv4}`. The UUIDv4 suffix is an implementation detail of [[?c_flow_identify]]; only the prefix is the relied-upon contract for classification.
- No input validation at construction; empty or whitespace-only strings are valid identities, though empty-string identities produce empty metric labels.
- Distinct strings are distinct identities — no aliasing or deduplication at the identity level.
- No lifecycle hooks or expiration semantics; an identity exists as long as the value holding it exists.

## Rationale

FlowId trades validation and strictness for simplicity. The prefix convention provides sufficient classification for observability without introducing state management overhead.

- Unvalidated construction keeps identity creation fast; callers control correctness.
- Prefix-based ephemeral classification is sufficient to bound metric cardinality without storing per-identity origin state.
- Exact string equality avoids ambiguity — callers see precisely what they constructed, with no silent normalization.
- Immutability guarantees stability as a lookup key in associative collections; identity tokens can be safely held across request boundaries.
- Debug wrapper format `FlowId(<string>)` preserves type context in developer-facing output while Display provides clean verbatim strings for production logs.

## Related

- [[?c_flow_identify]] — Request-time resolution of FlowId from HTTP request context, including header extraction, body metadata parsing, empty-value rejection, and auto-generated ephemeral ID generation.
- [[?c_flow_registry]] — FlowId serves as the lookup key for the flow registry; see [[?c_flow_registry]] for registration, lookup, and lifecycle semantics covering `Flow`, `FlowRegistration`, and `FlowRegistry`.
- [[?c_flow]] — FlowId labels the logical client within a [[?c_flow]] scheduling unit.
- [[?c_queue_snapshot]] — FlowId identities are rendered via Display for snapshot observability.
- [[src/flow/mod.rs#FlowId]] — FlowId definition and trait implementations.
