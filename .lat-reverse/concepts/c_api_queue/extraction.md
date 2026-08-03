# Extraction: c_api_queue

Source: `src/api/queue.rs`

## Responsibilities

- Exposes queue state via an HTTP endpoint
- Defines the JSON response shape for queue queries
- Reads queue data from `AppState.scheduler`

## Interface Surfaces

### HTTP Endpoint: GET /queue

- **Handler**: `queue_handler` (line 31)
- **Request**: No body, no query parameters
- **Response**: `Json<QueueResponse>` — always 200 OK; no error status codes emitted
- **Auth**: None required

### Exported Type: QueueResponse (lines 9-17)

- `active: u64` — count of flows currently executing at the backend
- `waiting: u64` — count of requests currently queued
- `flows: Vec<FlowPosition>` — per-flow entries for queued (not active) flows

### Exported Type: FlowPosition (lines 21-25)

- `id: String` — flow identifier
- `position: u64` — 1-indexed queue position

### Exported Function: queue_handler (line 31)

- **Precondition**: Receives `State<AppState>` containing a valid `scheduler`
- **Postcondition**: Returns a JSON-encoded `QueueResponse`
- **Error contract**: No `Result` wrapper; no HTTP error path

## Invariants

- **Position is 1-indexed**: `position` field starts at 1, never 0 (doc comment lines 15, 23)
- **Flows array is ordered by queue position**: `flows` entries appear in ascending position order (line 16)
- **Flows excludes active flows**: Only currently queued flows appear in `flows`; active flows are counted in `active` but not listed (line 15)
- **Response schema is stable**: Every response contains exactly `active`, `waiting`, and `flows` fields (lines 9-17)

## Failure Modes

- **No HTTP error path**: The handler returns `Json<QueueResponse>` directly, not a `Result`; callers cannot receive an error status code from this endpoint (line 31)
- **Silent failure on scheduler error**: If `state.scheduler.queue_snapshot()` panics or returns invalid data, the failure manifests as a server error, not a structured HTTP response (line 32)
