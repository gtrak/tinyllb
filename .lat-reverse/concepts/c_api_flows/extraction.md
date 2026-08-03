# API Flow Registration — Extraction

Source: `[[src/api/flows.rs]]`

## Responsibilities

- Accepts flow registration requests via HTTP and validates weight/priority constraints.
- Upserts flows into the registry (creates new, updates existing by id).
- Reports creation vs. update outcome to the caller.

## Interface Surface

### POST /flows — Register or update a flow

- **Request**: JSON body with `id` (string), `weight` (number), `priority` (integer).
- **Success responses**:
  - `201 Created` with `"status": "created"` when the flow id did not previously exist.
  - `200 OK` with `"status": "updated"` when the flow id already existed.
- **Response body**: JSON with `id`, `weight`, `priority`, `status` string.
- **Error responses**:
  - `400 Bad Request` with error message string when validation fails.
- **Evidence**: Lines 30-79 in `[[src/api/flows.rs]]`

### RegisterFlowRequest (public type)

- Carries client-supplied flow registration parameters.
- Evidence: Lines 10-18 in `[[src/api/flows.rs]]`

### RegisterFlowResponse (public type)

- Echoes submitted values and includes a `status` discriminator.
- Evidence: Lines 21-28 in `[[src/api/flows.rs]]`

## Invariants

- **Weight positivity**: `weight` must be strictly greater than zero. Rejected otherwise. Evidence: line 40 — `if req.weight <= 0.0`.
- **Priority range**: `priority` must be in the inclusive range [0, 100]. Values above 100 are rejected. Evidence: line 48 — `if req.priority > 100`.
- **Upsert semantics**: A flow with an existing id is updated rather than rejected. Evidence: line 63 — status is determined by `is_new` from registry.
- **Status discriminates outcome**: The response `status` field is `"created"` for new flows and `"updated"` for existing flows. Evidence: lines 72-76.

## Failure Modes

- **Invalid weight**: `weight <= 0` produces `400 Bad Request` with message `"weight must be greater than 0"`. Evidence: lines 40-45.
- **Invalid priority**: `priority > 100` produces `400 Bad Request` with message `"priority must be between 0 and 100"`. Evidence: lines 48-53.
