# c_api_router — Spec

## Purpose

The control-plane API router provides the administrative HTTP interface for flow scheduling configuration and queue observability. It assembles a fixed set of admin endpoints parameterized over application state, separating control-plane operations from user-facing inference traffic.

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

### Flow registration endpoint

- Accepts a flow identifier, scheduling weight, and priority class as input.
- Returns `201 Created` with status `"created"` when the flow is registered for the first time.
- Returns `200 OK` with status `"updated"` when an existing flow is modified.
- Returns `400 Bad Request` when weight or priority violate their domain bounds.

### Queue state endpoint

- Returns the current queue snapshot containing active flow count, waiting request count, and per-flow queue positions.
- Always returns `200 OK` with a complete queue snapshot; no error responses are produced.
- Queue positions are 1-indexed and reflect the current ordering in the queue.

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
- Flow registration supports only upsert semantics via a single POST method; no separate create or update paths exist.
- Validation errors are returned immediately without partial state changes.
- Queue positions are expressed as unsigned integers with no fractional or negative values.

## Rationale

Design choices follow from operational and correctness requirements.

- Separating admin endpoints from inference traffic prevents control-plane noise on high-throughput paths.
- Upsert semantics for flow registration simplify client logic: callers do not need to distinguish between create and update flows.
- Immediate validation rejection prevents invalid scheduling parameters from entering the system.
- The queue endpoint never errors to guarantee monitoring systems always receive data, even during degraded states.

## Related

- [[src/api/mod.rs#create_router]]
- [[src/api/flows.rs#RegisterFlowRequest]]
- [[src/api/flows.rs#register_handler]]
- [[src/api/queue.rs#QueueResponse]]
- [[src/api/queue.rs#queue_handler]]
