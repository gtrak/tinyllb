# App Composition

Source: `src/main.rs`

## Responsibilities

- Compose the HTTP router from four sub-routers (health, metrics, gateway, admin) and bind shared application state to each
- Resolve the server bind address from environment or configuration
- Load configuration, construct all core services, and wire them into shared application state
- Spawn background tasks (token-rate gauge updater, optional backend monitor) before starting the HTTP server
- Bind a TCP listener and hand control to the axum server for the lifetime of the process

## Interface Surfaces

### Main entry

- `#[tokio::main] async fn main()` — Single-process entry point; runs to completion or panic. No CLI arguments accepted. Evidence: `src/main.rs` lines 60-138.

### Router composition

- `pub fn create_router(state: gateway::AppState) -> Router` — Accepts fully wired `AppState`, returns a merged axum `Router`. Callable outside `main`. Evidence: `src/main.rs` lines 13-26.

### Dependency wiring

- `AppState` constructed in `main` with: `client`, `backend_url`, `metrics`, `scheduler`, `flow_registry`, `backpressure`, `request_timeout`. Passed to `create_router`, cloned into sub-router `.with_state()` calls. Evidence: `src/main.rs` lines 118-126, 17-19.

### Server startup

- `TcpListener::bind(&addr)` then `axum::serve(listener, app)`. Server runs until fatal error. No graceful shutdown hook. Evidence: `src/main.rs` lines 135-138.

### Port resolution

- Three-way precedence: (1) `LLM_QDISC__SERVER__BIND` env var present → use `cfg.server.bind` as-is; (2) `PORT` env var present → parse as `u16`, construct `0.0.0.0:<port>`; (3) fallback → `cfg.server.bind`. Evidence: `src/main.rs` lines 67-76.

## Invariants

- The router always contains exactly four merged sub-routers in fixed order: health, metrics, gateway, admin. Evidence: `src/main.rs` lines 21-25.
- `/healthz` responds `200` with body `"ok"` regardless of application state. Evidence: `src/main.rs` lines 9-11, test assertion lines 182-187.
- `/metrics` requires `AppState` to be set on its router. Evidence: `src/main.rs` line 17 (`.with_state(state.clone())`).
- All non-health sub-routers share the same `AppState` instance. Evidence: `src/main.rs` lines 17-19.
- The token-rate task never terminates; it loops indefinitely sleeping 1s between samples. Evidence: `src/main.rs` lines 40-56.
- `window_secs` used for rolling average is clamped to minimum 1. Evidence: `src/main.rs` line 35 (`window_secs.max(1)`).
- Configuration loading is a hard requirement; the process cannot start without it. Evidence: `src/main.rs` line 64 (`.expect("failed to load configuration")`).
- Telemetry is initialized before any other startup step. Evidence: `src/main.rs` line 62 (first statement in `main`).
- KV policy determines monitor type: enabled → `BackendMonitor::new` with optional background task; disabled → `BackendMonitor::empty`. Evidence: `src/main.rs` lines 87-101.

## Failure Modes

- Configuration load failure → process panics. Evidence: `src/main.rs` line 64 (`.expect`).
- `PORT` env var is not a valid `u16` → process panics. Evidence: `src/main.rs` line 70 (`.expect("PORT must be a valid port number")`).
- `0.0.0.0:<port>` fails to parse as `SocketAddr` → process panics. Evidence: `src/main.rs` line 73 (`.unwrap()`).
- TCP listener bind failure (port in use, invalid address) → process panics. Evidence: `src/main.rs` line 137 (`.unwrap()`).
- `axum::serve` failure → process panics. Evidence: `src/main.rs` line 138 (`.unwrap()`).
