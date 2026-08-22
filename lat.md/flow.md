# Flow Identifier Contract

FlowId is an opaque identity token that labels a logical client or workload whose requests are scheduled together. It classifies identities into ephemeral and named classes and maps each to a metric label.

## Purpose

FlowId provides construction, classification, metric-label derivation, equality semantics, and string rendering for identity values.

- Uniquely identifies a scheduling unit (logical client or workload).
- Classifies identity into two mutually exclusive classes: ephemeral and named.
- Maps each identity to a metric label, collapsing ephemeral identities to a single aggregate label.
- Supports exact equality and hashing for use as a lookup key.
- Renders the underlying string verbatim for observability surfaces.

## Non-goals

FlowId does not validate, normalize, or sanitize the strings it wraps. Classification uses a simple prefix convention and leaves correctness to callers.

- Does not validate input strings (empty, whitespace, or arbitrary content are accepted).
- Does not canonicalize, trim, or case-fold identifiers.
- Does not provide an independent marker distinguishing auto-generated ephemeral IDs from user-chosen ones sharing the same prefix.
- Request-time resolution of identities belongs to [[flow#Flow Identification]].
- Does not track creation time, origin, or lifecycle state beyond the identity string.

## Interface

The interface provides construction, classification, metric-label derivation, equality semantics, and string rendering for FlowId values.

- **Direct construction** — accepts any string-like value; produces a stable identity. No validation or error path.
- **Classification** — determines whether an identity belongs to the ephemeral class; pure function of the underlying string.
- **Metric label derivation** — ephemeral identities yield `"ephemeral"`; named identities yield their own string; empty-string identities yield empty labels.
- **Equality and hashing** — exact string equality, reflexive, symmetric, transitive. Hashing consistent with equality. Cloning yields an independent but equal value.
- **Rendering** — Display renders the underlying string verbatim. Debug renders as `FlowId(<string>)`.

## Invariants

The identity value is immutable after construction; all derived properties are stable for its lifetime.

- The underlying string never changes; there is no mutation API.
- Classification is a pure function of the underlying string; never changes for a given identity.
- An identity yields the aggregate metric label if and only if it is ephemeral.
- Display rendering reproduces the construction string exactly.
- Equality is exact string equality — no canonicalization, normalization, or case folding.

## Constraints

Classification and metric behavior are governed entirely by a string prefix convention. No independent state or attribute distinguishes identity classes.

- Ephemeral classification is determined solely by an `ephemeral-` string prefix; user-chosen names with this prefix are treated as ephemeral.
- Auto-generated ephemeral IDs use format `ephemeral-{UUIDv4}`. Only the prefix is the relied-upon contract for classification.
- No input validation; empty or whitespace-only strings are valid.
- Distinct strings are distinct identities — no aliasing or deduplication.
- No lifecycle hooks or expiration semantics.

## Rationale

FlowId trades validation for simplicity. The prefix convention provides sufficient classification for observability without introducing state management overhead.

- Unvalidated construction keeps identity creation fast; callers control correctness.
- Prefix-based ephemeral classification bounds metric cardinality without per-identity state.
- Exact string equality avoids ambiguity — callers see precisely what they constructed.
- Immutability guarantees stability as a lookup key in associative collections.
- Debug wrapper preserves type context while Display provides clean verbatim strings.

## Related

Related concepts and source code for identity semantics.

- [[flow#Flow Identification]] — Request-time resolution of FlowId from HTTP request context, including header extraction, body metadata parsing, empty-value rejection, and auto-generated ephemeral ID generation.
- [[flow#Flow Registry and State]] — FlowId serves as the lookup key for the flow registry; see [[flow#Flow Registry and State]] for registration, lookup, and lifecycle semantics.
- [[flow#Flow Registry and State]] — FlowId labels the logical client within a scheduling unit.
- [[gateway#Session Slot Pinning]] — a named flow id also selects a stable llama.cpp backend slot via a deterministic hash.
- FlowId identities are rendered via Display for snapshot observability.
- [[src/flow/mod.rs#FlowId]] — FlowId definition and trait implementations.

# Flow Registry and State

The flow registry is the authoritative source of scheduling-entity state. It maps every flow identity to exactly one registered entry and provides concurrent access to independently usable flow references.

## Purpose

The registry guarantees that every flow identity maps to exactly one registered entry and provides concurrent access to independently usable flow references.

- Supplies creation-time defaults, weight and priority upserts, aggregate depth queries, and queue snapshots.
- Maintains per-flow attributes that scheduling policy reads without coordinating access.
- The registry itself does not manage scheduling policy.
- Consumers depend on it for flow identity, attribute access, and queue snapshots.

## Non-goals

The registry is not a queue and does not define ordering, validate values, or coordinate counter updates with scheduling decisions.

- Does not define ordering among waiting flows; ordering is supplied externally.
- Does not validate weight or priority ranges.
- Does not enforce scheduling policy or coordinate depth and credit counter updates.
- Provides no explicit unregistration API; removal happens only through the idle reaper, which evicts flows with zero depth, zero active requests, and a stale last-seen timestamp (see [[app#Idle-Flow Reaper]]).
- Active-count mechanism tracks in-flight presence but provides no underflow protection.

## Interface

The registry exposes contractual surfaces covering construction, registration, lookup, aggregate queries, snapshots, per-flow attributes, and flow identity.

- **Construction** — instantiated with default weight and priority; defaults apply only to flows created through lookup.
- **Registration payload** — `FlowRegistration` with public fields: identity, weight, priority.
- **Registration** — creates a new entry or updates weight and priority; always succeeds and reports whether insertion occurred.
- **Lookup** — returns an independently usable shared flow reference; auto-creates with defaults if not registered.
- **Aggregate queries** — reports registered flow count, emptiness, and sum of all per-flow depth counters.
- **Queue snapshots** — produces a snapshot with global counts and ordered `QueueFlowEntry` items, filtering to registered flows with positive depth.
- **Per-flow attributes** — weight, priority, and priority source (readable/writable methods); depth, credit, enqueued timestamp, active count (direct public field access). Priority source indicates whether priority was set by the heuristic (0), an explicit header (1), or the admin API (2).
- **Priority overrides** — `apply_priority_override` pins a flow's priority to a class value from the `X-LLM-Priority` header (source 1) or, on `auto`, clears the pin (source 0) and resets priority to the configured `agent` class.
- **Idle eviction support** — a `last_seen` timestamp (unix seconds) is refreshed on lookup and registration; the active count is tracked via `inc_active`/`dec_active`/`is_active` accessors; together these feed the idle reaper (see [[app#Idle-Flow Reaper]]).
- **Flow identity** — opaque type with string construction, equality, display, ephemeral classification, and metric label derivation.

## Invariants

All statements about the registry remain true regardless of implementation details.

- Each flow identity maps to at most one registered entry; creation paths never produce duplicates.
- A flow remains registered until the idle reaper evicts it: entries with zero depth, zero active requests, and a last-seen timestamp older than the configured TTL are removed; there is no explicit unregistration API.
- Weight, priority, credit, and depth are updated individually; no cross-attribute atomicity is guaranteed.
- Snapshots list only flows with positive depth, contain no duplicates, and assign contiguous 1-based positions.
- Ephemeral metric label always resolves to a single common value; named labels equal the identity string.
- Priority source is an independent attribute from priority value; updating one does not atomically update the other. The heuristic checks priority source before writing priority and skips flows with non-zero source.

## Constraints

The registry operates under explicit limitations that shape its safe usage.

- Weight and priority updates are not mutually exclusive with concurrent reads; consumers may observe briefly inconsistent attribute pairs.
- Active-count decrement uses wrapping atomic subtraction in all build profiles; an underflow wraps to the maximum representable value and never panics.
- Aggregate depth sum may overflow 32-bit range under extreme depth; overflow behavior depends on compilation profile.
- Snapshot positions reflect relative order among included flows only; position is not an absolute queue index.
- Snapshot global counts are caller-supplied and not cross-checked against per-flow data.

## Rationale

A centralized registry separates identity management from scheduling logic, enabling concurrent access and fine-grained metric tracking.

- Separates identity management from scheduling; scheduling reads per-flow state without coordination.
- Concurrent access to flow references avoids serializing all consumers through a single point.
- Ephemeral-vs-named distinction enables coarse-grained aggregation for anonymous workloads.
- Defaults on lookup support dynamic discovery; explicit registration allows policy-level control.
- Direct field access on counters enables subsystems to update without indirection.

## Related

Related concepts and source code for flow registry semantics.

- [[flow#Flow Identifier Contract]] — Identity semantics and ephemeral classification.
- Consumer that reads weight, priority, depth, and credit from registered flows.
- Queue snapshot surface and its interpretation.
- [[src/flow/mod.rs#FlowRegistry]] — Registry implementation.
- [[src/flow/mod.rs#Flow]] — Per-flow attribute storage.
- [[src/flow/mod.rs#FlowId]] — Identity and label derivation.

# Flow Identification

Every incoming request resolves to exactly one flow identifier, enabling downstream systems to attribute the request to a logical client or workload.

## Purpose

Resolution guarantees every request produces a flow identifier using multiple sources with fixed precedence. No consumer path observes an absent or unresolved identifier.

- Recognizes an explicit override header, harness session headers (Claude Code, opencode, pi), JSON body metadata, and an auto-generated fallback.
- Fixed precedence order determines which source wins when multiple supply values.
- Harness session headers group all requests of one agentic session into a single stable flow instead of per-request ephemeral identifiers.
- Opaque identifier type distinguishes user-supplied from auto-generated ephemeral identifiers.
- Ephemeral identifiers collapse to a single metric label, preventing unbounded cardinality.

## Non-goals

Flow identification resolves the identifier; it does not interpret, validate, or manage it.

- Semantic correctness of a client-supplied identifier is not verified.
- Authentication, authorization, and rate limiting based on the identifier are not performed.
- Identifier lifetime, expiration, and cross-request consistency are not tracked.
- Non-JSON body formats are not parsed for identifiers.
- Subagent correlation identifiers are not used for flow identity; subagent requests are scheduled with their session.

## Interface

The resolution contract accepts pre-extracted request headers and body bytes, and produces a FlowId guaranteed to always succeed.

- **Resolution input** — caller provides headers and body separately; both optional; absence triggers auto-generated fallback.
- **Precedence** — `X-LLM-Flow-ID` header first, then harness session headers in order, then JSON body `metadata.flow_id`, then auto-generated ephemeral identifier:
  1. `X-LLM-Flow-ID` (explicit override)
  2. `x-claude-code-session-id` (Claude Code)
  3. `x-session-id` (de-facto standard: opencode, pi, vLLM, Anthropic-compatible proxy convention)
  4. `x-session-affinity` (opencode, pi)
  5. `x-client-request-id` (pi, Codex OpenAI-compatible paths)
  6. `session_id` (pi, Codex Responses wire header)
  7. `metadata.flow_id` (JSON body)
  8. Auto-generated `ephemeral-{UUIDv4}`
- **Source acceptance** — accepted per source: the `X-LLM-Flow-ID` override header must be valid UTF-8 and non-empty, but is not trimmed — a whitespace-only override value is adopted verbatim as the flow identifier. Harness session header names are matched case-insensitively and their values are trimmed, so a whitespace-only session value is treated as absent. The JSON body must be parseable with a `metadata` object containing a non-empty `flow_id` string.
- **FlowId constructor** — accepts any string, including empty and `ephemeral-` prefixed.
- **FlowId Display** — yields the exact underlying string value.
- **Ephemeral test** — returns true when identifier begins with `ephemeral-`.
- **Metric label** — ephemeral yields `"ephemeral"`; named yields exact value.
- **Resolution output** — returns a `ResolvedFlow` containing the resolved flow identifier, an optional priority class override, and an unset-override flag. The flow identifier is always present; the override fields are `None`/`false` when no `X-LLM-Priority` header is sent.
- **Priority header** — `X-LLM-Priority` header is parsed case-insensitively alongside flow identity resolution. Values `interactive`, `agent`, `background` produce a priority class override; `auto` signals clearing any prior override; unknown values are logged as warnings and ignored. The header does not affect flow identity resolution.

## Invariants

The following properties hold regardless of implementation changes.

- Every request yields exactly one flow identifier; no unresolved or error outcome.
- When the first source in precedence yields a usable value, it exclusively determines the result.
- If no source supplies a usable identifier, the result is an auto-generated ephemeral identifier.
- Each invocation of auto-generation produces a statistically distinct identifier.
- Ephemeral classification is determined precisely by the `ephemeral-` prefix.
- Requests sharing a harness session identifier resolve to the same flow identifier.
- The `X-LLM-Priority` header, when present, does not alter the resolved flow identifier; it only attaches priority override metadata to the result.

## Constraints

The identification contract operates within strict boundaries on input acceptance and output classification.

- Empty values are treated as absent for the session headers and body source, and for the `X-LLM-Flow-ID` override header only the strictly-empty string is rejected — the override is not trimmed, so a whitespace-only override value is adopted verbatim.
- Body source requires specific JSON structure: `metadata` object with string-valued `flow_id`.
- Sources that fail are silently skipped; no diagnostic is emitted and no error propagates.
- No server-side cross-request persistence; repeated requests resolve to the same flow only when the client repeats an identifier.
- User-supplied identifiers beginning with `ephemeral-` are indistinguishable from auto-generated ones.
- Parent/agent identifiers (`x-parent-session-id`, `x-claude-code-agent-id`, `x-claude-code-parent-agent-id`) are not identity sources; they remain available for trace attribution.

## Rationale

Fixed precedence and guaranteed resolution exist to make downstream attribution deterministic and safe.

- Fixed precedence eliminates ambiguity: callers know exactly which source dominates.
- Harness session headers let one agentic conversation share a flow, so fair scheduling and metrics operate on sessions rather than single requests.
- Guaranteed resolution prevents downstream systems from handling null-identifier paths.
- Rejecting empty strings avoids adopting values with no attribution signal.
- Silently skipping unusable sources prevents malformed requests from surfacing errors.
- Collapsing ephemeral identifiers prevents unbounded metric cardinality.

## Related

Related concepts and source code for flow identification.

- Flow context management that consumes the resolved identifier.
- Metric aggregation using flow identifier labels.
- [[src/flow/identify.rs]] — Resolution implementation, including harness session-header extraction.
- [[src/flow/mod.rs#PriorityClass]] — priority class enumeration and token resolution
- [[src/flow/mod.rs#ResolvedFlow]] — resolved flow identity and priority override
- [[src/flow/cadence.rs]] — cadence-based priority heuristic
- [[src/flow/mod.rs]] — Flow identifier type and ephemeral classification.

# Cadence-Based Priority Heuristic

A turn-boundary state machine classifies flows as interactive or agentic from request-arrival cadence and adjusts flow priority automatically.

## Purpose

The heuristic replaces static priority defaults with per-flow classification that reacts to how a flow actually sends requests.

- Four `CadenceState` values — Cold, Interactive, AgenticSuspected, AgenticConfirmed — map to configured priority classes via `Priorities`: Cold and Interactive map to `interactive` (default 100), AgenticSuspected to `agent` (default 50), AgenticConfirmed to `background` (default 10).
- Promotion: a turn-boundary arrival (a `role: user` request) whose gap since the previous arrival is at least `priority_policy.idle_gap_threshold` immediately promotes the flow to Interactive regardless of prior state, and resets the continuous-arrival counter.
- Demotion: consecutive non-turn-boundary arrivals increment `continuous_arrival_count`; crossing `agentic_suspected_threshold` demotes the flow to AgenticSuspected, and crossing `agentic_confirmed_threshold` demotes it to AgenticConfirmed.
- A fast turn-boundary arrival (a user request with a gap below the threshold) resets the continuous-arrival counter without promoting the flow.
- Flows with `priority_source != 0` (a header or admin override pinned the priority) are never modified by the state machine; `priority_policy.enabled = false` acts as a kill switch that suppresses all priority changes.
- `state_of(flow_id)` exposes the current state for the `llm_flow_cadence_state` metric (see [[metrics#Metric Family Contracts]]).
- `CadenceRegistry::reap_idle(ttl)` evicts entries whose last arrival is older than the TTL, keeping the cadence registry bounded (see [[app#Idle-Flow Reaper]]).

## Related

Cross-references to related concepts and source locations for the cadence-based priority heuristic.

- [[src/flow/cadence.rs]] — State machine and registry implementation
- [[gateway#Turn-Boundary Detection]] — Turn-boundary signal that drives promotion
- [[scheduler#Scheduler Facade and Policy Selection]] — Admission path that applies the classified priority
- [[config#Configuration Contract]] — `priority_policy` and `priorities` configuration
- [[metrics#Metric Family Contracts]] — Cadence state and priority metrics
- [[app#Idle-Flow Reaper]] — Eviction of stale cadence entries
