# API Flow Registration

## Purpose

This concept defines the contract for registering scheduling flows into a flow registry. It guarantees that callers can create or update flow configurations with validated parameters, and receive unambiguous feedback about the outcome. The interface ensures parameter integrity and identity-based resolution for every registration attempt.

- Guarantees flow configurations are persisted with validated weight and priority values.
- Resolves registration attempts by flow identity, distinguishing new creations from updates to existing entries.
- Reports the outcome classification (newly created or updated) to every successful caller.
- Rejects registrations whose parameters violate bounds.

## Non-goals

This concept does not cover flow execution, scheduling decisions, or flow removal.

- Does not define how registered weights or priorities influence scheduling behavior.
- Does not provide a mechanism to delete or deactivate flows.
- Does not expose read-only retrieval of flow configurations.

## Interface

The public surface is a single HTTP endpoint with explicit contracts for request acceptance, response classification, and error handling.

- **POST /flows** — Accepts a flow identity, scheduling weight, and priority class. Returns the echoed parameters with a status discriminator and one of two success-level HTTP status codes.
- **HTTP 201 Created** — Returned when the supplied flow identity does not match any existing entry; response body contains `"status": "created"`.
- **HTTP 200 OK** — Returned when the supplied flow identity matches an existing entry; response body contains `"status": "updated"`.
- **HTTP 400 Bad Request** — Returned when weight or priority violates bounds; response body is a plain-text error message.
- **Response body** — On success, echoes the submitted `id`, `weight`, and `priority`, and includes a `status` field discriminating `"created"` versus `"updated"`.

## Invariants

All stated conditions hold regardless of implementation details.

- **Weight positivity** — Every successfully registered flow has `weight > 0`.
- **Priority bounds** — Every successfully registered flow has `priority` in the inclusive range `[0, 100]`.
- **Identity-based resolution** — A flow identity that already exists is updated rather than rejected or duplicated.
- **Status matches outcome** — The response `status` field reflects the actual resolution: `"created"` only for previously unknown identities, `"updated"` only for existing ones.
- **Echo consistency** — The response body mirrors the exact `id`, `weight`, and `priority` supplied by the caller.

## Constraints

These boundaries limit the scope of the interface.

- Accepts only JSON request bodies.
- Weight must be a positive floating-point number; zero and negative values are rejected.
- Priority must be an integer within `[0, 100]`; values exceeding 100 are rejected.
- No authentication or authorization is required to register flows.

## Rationale

Flow registration is separated from scheduling to allow independent configuration management.

- Identity-based upsert semantics let callers safely retry or re-register without manual existence checks.
- Explicit status codes (201 vs 200) and body discriminators (`"created"` vs `"updated"`) provide independent signals for both protocol-level and payload-level consumers.
- Tight parameter bounds are enforced at the edge to prevent invalid configurations from entering the registry.

## Related

- [[?flow-registry]] — Underlying registry that stores flow configurations.
- [[?scheduling]] — Downstream consumer of registered weights and priorities.
- `[[src/api/flows.rs]]` — Implementation of the registration endpoint and request/response types.
