# c_api_router — Extraction

## Responsibilities

- Exposes two public submodules: `flows` and `queue` (mod.rs lines 1-2).
- Provides a single factory function that assembles an admin router mounting exactly two endpoints (mod.rs lines 14-18).
- The router is parameterized over `AppState` (mod.rs line 14, line 7).

## Interface Surfaces

### Router factory: `create_router() -> Router<AppState>`

- Mounts `POST /flows` → `flows::register_handler` (mod.rs line 16).
- Mounts `GET /queue` → `queue::queue_handler` (mod.rs line 17).

### `POST /flows` — Flow registration / update

- **Request body** (`RegisterFlowRequest`, flows.rs lines 11-18):
  - `id: String` — flow identifier.
  - `weight: f64` — scheduling weight.
  - `priority: u32` — priority class.
- **Response body** (`RegisterFlowResponse`, flows.rs lines 22-28):
  - `id: String`, `weight: f64`, `priority: u32`, `status: String`.
  - `status` is `"created"` for new flows, `"updated"` for existing flows (flows.rs lines 72-76).
- **Status codes** (flows.rs lines 35-38, 62-67):
  - `201 Created` — flow registered for the first time (flows.rs line 64).
  - `200 OK` — existing flow updated (flows.rs line 66).
  - `400 Bad Request` — validation failure (flows.rs lines 40-44, 48-52).

### `GET /queue` — Queue state snapshot

- **Response body** (`QueueResponse`, queue.rs lines 9-17):
  - `active: u64` — currently executing flows (queue.rs line 11).
  - `waiting: u64` — requests waiting in queue (queue.rs line 13).
  - `flows: Vec<FlowPosition>` — per-flow positions (queue.rs line 16).
- **`FlowPosition`** (queue.rs lines 21-25):
  - `id: String`, `position: u64` (1-indexed, queue.rs line 24).
- **Status codes**: always `200 OK` (queue.rs line 31 — returns `Json<QueueResponse>` directly, no error path).

## Invariants

- The router mounts exactly two endpoints: `/flows` (POST) and `/queue` (GET) (mod.rs lines 15-17).
- Weight must be strictly greater than 0; values `<= 0` are rejected with `400` (flows.rs lines 40-44).
- Priority must be in `[0, 100]`; values `> 100` are rejected with `400` (flows.rs lines 48-52).
- `status` field in `POST /flows` response is deterministic: `"created"` when the flow is new, `"updated"` when it already existed (flows.rs lines 63-67, 72-76).
- Queue positions in `GET /queue` are 1-indexed and ordered by queue position (queue.rs lines 15-16).
- `GET /queue` never returns an error response; it always produces a `200 OK` with `QueueResponse` (queue.rs line 31).

## Failure Modes

- `POST /flows` returns `400 Bad Request` when `weight <= 0` (flows.rs lines 40-44).
- `POST /flows` returns `400 Bad Request` when `priority > 100` (flows.rs lines 48-52).
- `POST /flows` returns an `Err` tuple `(StatusCode, String)` on validation failure; success returns `Ok` with `StatusCode + Json<RegisterFlowResponse>` (flows.rs line 38).
- `GET /queue` has no observable failure mode; the handler signature returns `Json<QueueResponse>` directly with no error path (queue.rs line 31).
