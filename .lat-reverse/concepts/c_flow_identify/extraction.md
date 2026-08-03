# c_flow_identify — Extraction

## Responsibilities

- Derive a single stable flow identifier from an incoming HTTP request so the
  request can be attributed to a logical client or workload.
- Resolve the identifier by consulting request sources in a fixed precedence
  order (header, then JSON body, then auto-generated), guaranteeing that a
  resolution always succeeds (never returns "no identifier").

## Interface surfaces

### Flow identification contract (request → flow id)

- **Inputs:** the HTTP request headers and the raw request body bytes.
- **Outputs:** a `FlowId` identifying the flow the request belongs to.
  Guaranteed to always be produced (resolution never fails/dead-ends).
- **Precedence contract (highest first):**
  1. A non-empty value in the `X-LLM-Flow-ID` request header.
  2. A non-empty string-valued `flow_id` under `metadata` in a JSON body.
  3. An auto-generated identifier.
- **Postcondition:** the chosen identifier is either (a) the exact string taken
  from a request source, or (b) an auto-generated marker.
- **Errors:** no error channel to the caller; lower-precedence sources are
  silently skipped when a higher-precedence source is present or when a source
  fails to yield a usable value.

Evidence: `pub fn resolve` at `src/flow/identify.rs:16`.

### Sub-surface: header-derived flow id

- **Inputs:** request headers.
- **Preconditions:** header `X-LLM-Flow-ID` present and its value decomposes to
  a non-empty string (empty string is rejected).
- **Postconditions (when present):** yields the exact header value as the flow
  identifier.
- **Failure/skip condition:** header absent, not decodable as a string, or
  empty → no identifier contributed.
- **Precedence:** consulted first; its result short-circuits all other sources.

Evidence: `src/flow/identify.rs:36`.

### Sub-surface: body-derived flow id

- **Inputs:** raw request body bytes.
- **Preconditions:** body non-empty, parses as JSON, contains `metadata` object
  whose `flow_id` member is a non-empty string.
- **Postcondition:** yields the `metadata.flow_id` string as the identifier.
- **Failure/skip:** empty body, unparseable JSON, missing `metadata`, missing or
  non-string `flow_id`, or empty `flow_id` value → no identifier contributed.
- **Evidence:** `src/flow/identify.rs:50`.

### Fallback: ephemeral identifier generation

- **Outputs:** an identifier that is unique per invocation and marked as
  ephemeral/auto-generated.
- **Postcondition:** successive calls yield distinct identifiers.
- Evidence: `src/flow/identify.rs:67`.

## Invariants

- A request always resolves to exactly one flow identifier; there is no
  "unresolved" outcome. (`resolve` returns `FlowId`, total function.)
- The header source, when usable, exclusively determines the result regardless
  of what the body contains. (`empty_header_falls_through` and
  `metadata_flow_id_is_extracted` and `header_takes_precedence_over_metadata`,
  `src/flow/identify.rs:88,98,108`)
- If no request source yields a value, the result is an auto-generated
  ephemeral identifier, not an error. (`src/flow/identify.rs:25,30`,
  `src/flow/identify.rs:120,128`)
- Auto-generated identifiers are unique across invocations. (`src/flow/identify.rs:137`)
- An identifier is classified ephemeral when it begins with the auto-generated
  marker prefix. (`FlowId::is_ephemeral`, `src/flow/mod.rs:24`)
- The metric label of an auto-generated identifier collapses to a single
  aggregate value; a named flow's label is its exact identifier. (`FlowId::metric_label`,
  `src/flow/mod.rs:32`)

## Failure modes

- **Mislabeled slice:** body flow-id extraction requires a valid JSON `metadata`
  wrapper; a body carrying a flow id in a different shape is silently ignored,
  yielding an auto-generated identifier instead.
- **Non-decodable header:** a header value that cannot be read as a string is
  silently skipped, yielding a lower-precedence or auto-generated identifier.
- **Empty-string value:** from the header or a JSON field is treated as absent,
  so an empty value is never adopted.