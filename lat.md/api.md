# Admin API Router Assembly

The control-plane API router assembles a fixed set of admin endpoints parameterized over application state, separating control-plane operations from user-facing inference traffic.

## Purpose

Supplies a single admin router instance exposing flow registration and queue inspection as HTTP endpoints, enforcing validation boundaries on scheduling parameters at the edge.

- Supplies a single, self-contained admin router instance.
- Exposes flow registration and queue inspection as HTTP endpoints.
- Parameterized over application state to access scheduling and queue internals.
- Enforces validation boundaries on scheduling parameters at the edge.

## Non-goals

The router does not address inference workloads or user-facing protocols.

- Does not expose model inference, token streaming, or `/v1/`-compatible endpoints.
- Does not perform authentication or authorization.
- Does not implement flow scheduling logic or execution ordering.
- Does not support flow deletion or bulk configuration operations.

## Interface

Each public endpoint carries a precise input contract, output guarantee, and error classification.

**Flow Registration**

- Accepts a flow identifier, scheduling weight, and priority class as input.
- Returns `201 Created` with status `"created"` for first-time registration; `200 OK` with `"updated"` for modifications.
- Returns `400 Bad Request` when weight or priority violate domain bounds.

**Queue State**

- Returns the current queue snapshot containing active flow count, waiting request count, and per-flow queue positions.
- Always returns `200 OK`; no error responses are produced.
- Queue positions are 1-indexed and reflect current ordering.

**Context Inspection** (see [[context#Context Compression]])

- `GET /admin/context` — list all flows with transcript metadata (token counts, compressed-segment count). Optional `?over_threshold=true` filter.
- `GET /admin/context/{flow_id}` — full segment breakdown for a flow with per-segment previews, token counts, and compression status.
- `POST /admin/context/{flow_id}/compress` — force-trigger compression for a flow. Returns `202 Accepted` with the turn range, or `409 Conflict` if nothing is compressible.
- `DELETE /admin/context/{flow_id}` — clear a flow's transcript. Returns `200 OK` or `404` if not found.
- All context endpoints return `503 Service Unavailable` when context compression is disabled.

## Invariants

These properties hold regardless of implementation changes.

- Weight must be strictly positive; zero and negative values are rejected.
- Priority must fall within the inclusive range `[0, 100]`; values outside this range are rejected.
- The `status` field in flow registration responses is deterministic: `"created"` for new flows, `"updated"` for existing flows.
- Queue positions are always 1-indexed and reflect actual queue ordering.
- Queue state endpoint produces no error responses under any condition.

## Constraints

These boundaries limit what the router can or cannot do.

- The router exposes exactly two endpoints: one for flow registration, one for queue state.
- Flow registration supports only upsert semantics via a single POST method.
- Validation errors are returned immediately without partial state changes.
- Queue positions are expressed as unsigned integers with no fractional or negative values.

## Rationale

Design choices follow from operational and correctness requirements.

- Separating admin endpoints from inference traffic prevents control-plane noise on high-throughput paths.
- Upsert semantics for flow registration simplify client logic: callers do not need to distinguish between create and update flows.
- Immediate validation rejection prevents invalid scheduling parameters from entering the system.
- The queue endpoint never errors to guarantee monitoring systems always receive data.

## Related

Related concepts and source locations for the admin API router.
- [[src/api/mod.rs#create_router]]
- [[src/api/flows.rs#RegisterFlowRequest]]
- [[src/api/flows.rs#register_handler]]
- [[src/api/queue.rs#QueueResponse]]
- [[src/api/queue.rs#queue_handler]]

# Flow Registration Endpoint

This endpoint defines the contract for registering scheduling flows into a flow registry, guaranteeing that callers can create or update flow configurations with validated parameters and receive unambiguous feedback.

## Purpose

Guarantees flow configurations are persisted with validated weight and priority values, resolves registration by flow identity, reports outcome classification, and rejects out-of-bounds parameters.

- Guarantees flow configurations are persisted with validated weight and priority values.
- Resolves registration attempts by flow identity, distinguishing new creations from updates.
- Reports the outcome classification (newly created or updated) to every successful caller.
- Rejects registrations whose parameters violate bounds.

## Non-goals

This concept does not cover flow execution, scheduling decisions, or flow removal.

- Does not define how registered weights or priorities influence scheduling behavior.
- Does not provide a mechanism to delete or deactivate flows.
- Does not expose read-only retrieval of flow configurations.

## Interface

A single HTTP endpoint with explicit contracts for request acceptance, response classification, and error handling.

- **POST /flows** — Accepts a flow identity, scheduling weight, and priority class. Returns echoed parameters with a status discriminator.
- **HTTP 201 Created** — Returned for previously unknown flow identities; response body contains `"status": "created"`.
- **HTTP 200 OK** — Returned for existing flow identities; response body contains `"status": "updated"`.
- **HTTP 400 Bad Request** — Returned when weight or priority violates bounds; response body is a plain-text error.

## Invariants

All stated conditions hold regardless of implementation details.

- **Weight positivity** — Every successfully registered flow has `weight > 0`.
- **Priority bounds** — Every successfully registered flow has `priority` in the inclusive range `[0, 100]`.
- **Identity-based resolution** — A flow identity that already exists is updated rather than rejected or duplicated.
- **Status matches outcome** — The response `status` field reflects the actual resolution: `"created"` only for new identities, `"updated"` only for existing ones.
- **Echo consistency** — The response body mirrors the exact `id`, `weight`, and `priority` supplied by the caller.

## Constraints

These boundaries limit the scope of the interface.

- Accepts only JSON request bodies.
- Weight must be a positive floating-point number; zero and negative values are rejected.
- Priority must be an integer within `[0, 100]`; values exceeding 100 are rejected.
- No authentication or authorization is required to register flows.

## Rationale

Flow registration is separated from scheduling to allow independent configuration management.

- Identity-based upsert semantics let callers safely retry without manual existence checks.
- Explicit status codes (201 vs 200) and body discriminators (`"created"` vs `"updated"`) provide independent signals for protocol-level and payload-level consumers.
- Tight parameter bounds are enforced at the edge to prevent invalid configurations from entering the registry.

## Related

Related concepts and source locations for flow registration.
- [[flow#Flow Registry and State]]
- [[scheduler#Scheduler Facade and Policy Selection]]
- [[src/api/flows.rs#register_handler]]

# Queue Status Endpoint

This endpoint provides external observability into the current state of the inference queue, guaranteeing consumers can query active count, waiting count, and per-flow positions.

## Purpose

Exposes a unified queue snapshot including active count, waiting count, and per-flow positions, answering "where is my flow in line?" while operating as a read-only observation surface.

- Exposes a unified queue snapshot including active count, waiting count, and per-flow positions.
- Answers "where is my flow in line?" for every waiting flow.
- Separates active flows (counted) from queued flows (listed with positions).
- Operates as a read-only observation surface; does not modify queue state.

## Non-goals

This concept deliberately excludes capabilities beyond queue observation.

- Does not enqueue, dequeue, or otherwise mutate the queue.
- Does not expose scheduler internals, configuration, or backend state.
- Does not provide historical queue data or trend information.
- Does not support filtering, pagination, or per-request querying.

## Interface

A single query contract that returns a complete queue snapshot.

- Accepts an unauthenticated request with no required parameters.
- Returns exactly three fields: active flow count, waiting flow count, and an ordered list of per-flow positions.
- Each per-flow position entry contains a flow identifier and a 1-indexed queue position.
- Every response succeeds with the same structural shape; no error status codes are defined.
- Position values are 1-indexed; position 1 means first in queue.

## Invariants

These statements hold regardless of implementation details.

- The active count plus waiting count equals the total number of flows represented in the snapshot.
- Per-flow positions are strictly ordered: position values form the sequence 1, 2, 3, ... up to the waiting count.
- Active flows never appear in the per-flow position list.

## Constraints

These boundaries define what the concept can and cannot guarantee.

- The snapshot reflects queue state at a single instant; staleness is not bounded by this concept.
- No authentication or authorization is required or enforced.
- The endpoint emits no HTTP error codes; internal failures are not exposed as structured responses.
- Position semantics are queue-local: they indicate ordering within the waiting queue, not global scheduling priority.

## Rationale

Queue observability is a prerequisite for user-facing status feedback and operational monitoring.

- Separating active count from waiting positions lets consumers distinguish throughput from backlog.
- 1-indexed positions match natural language and avoid zero-vs-one ambiguity.
- Providing a single-snapshot shape reduces coordination cost: one call yields a complete picture.
- Omitting error codes simplifies the consumer contract; failure modes are server-level, not protocol-level.

## Related

Related concepts and source locations for queue status endpoint.
- [[scheduler#Scheduler Facade and Policy Selection]]
- [[flow#Flow Registry and State]]
- [[gateway#Gateway Application State]]
- [[src/api/queue.rs#QueueResponse]]
- [[src/api/queue.rs#FlowPosition]]
