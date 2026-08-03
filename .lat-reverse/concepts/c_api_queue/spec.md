# Spec: c_api_queue

## Purpose

This concept provides external observability into the current state of the inference queue. It guarantees that any consumer can query how many work items are actively executing, how many are waiting, and the precise position of each waiting flow.

- Exposes a unified queue snapshot including active count, waiting count, and per-flow positions
- Answers "where is my flow in line?" for every waiting flow
- Separates active flows (counted) from queued flows (listed with positions)
- Operates as a read-only observation surface; does not modify queue state

## Non-goals

This concept deliberately excludes capabilities beyond queue observation.

- Does not enqueue, dequeue, or otherwise mutate the queue
- Does not expose scheduler internals, configuration, or backend state
- Does not provide historical queue data or trend information
- Does not support filtering, pagination, or per-request querying

## Interface

The public surface is a single query contract that returns a complete queue snapshot.

- Accepts an unauthenticated request with no required parameters
- Returns exactly three fields: active flow count, waiting flow count, and an ordered list of per-flow positions
- Each per-flow position entry contains a flow identifier for correlation and a 1-indexed queue position
- Every response succeeds with the same structural shape; no error status codes are defined
- Position values are 1-indexed; position 1 means first in queue

## Invariants

These statements hold regardless of implementation details.

- The active count plus waiting count equals the total number of flows represented in the snapshot
- Per-flow positions are strictly ordered: position values form the sequence 1, 2, 3, … up to the waiting count
- Active flows never appear in the per-flow position list

## Constraints

These boundaries define what the concept can and cannot guarantee.

- The snapshot reflects queue state at a single instant; staleness is not bounded by this concept
- No authentication or authorization is required or enforced
- The endpoint emits no HTTP error codes; internal failures are not exposed as structured responses
- Position semantics are queue-local: they indicate ordering within the waiting queue, not global scheduling priority

## Rationale

Queue observability is a prerequisite for user-facing status feedback and operational monitoring.

- Separating active count from waiting positions lets consumers distinguish throughput from backlog
- 1-indexed positions match natural language ("you are 3rd in line") and avoid zero-vs-one ambiguity
- Providing a single-snapshot shape reduces coordination cost: one call yields a complete picture
- Omitting error codes simplifies the consumer contract; failure modes are server-level, not protocol-level

## Related

- [[?scheduler]] — source of queue state; this concept observes its data
- [[?flow]] — the unit of work whose position appears in the response
- [[?gateway]] — hosting layer that exposes this endpoint
- [[src/api/queue.rs#QueueResponse]] — response type definition
- [[src/api/queue.rs#FlowPosition]] — per-flow position type definition
