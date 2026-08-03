# Issue 06 — Context State + AppState Integration

## Objective

Create the `ContextState` that ties together the store, token estimator,
config, and per-flow locking. Wire it into `AppState` so the proxy handler
and compression worker can access it. Initialize it at startup in `main.rs`.

## Files

| File | Change |
|------|--------|
| `src/context/mod.rs` | Add `ContextState` struct + constructor |
| `src/gateway/mod.rs` | Add `context: Arc<ContextState>` to `AppState` |
| `src/main.rs` | Initialize `ContextState` at startup, pass to `AppState` |

## Prerequisites

- Issue 01 (config — `ContextPolicy`)
- Issue 02 (token estimator)
- Issue 04 (SQLite store)
- Issue 05 (reconciliation)

## Steps

1. **`ContextState` struct** in `src/context/mod.rs`:
   ```rust
   pub struct ContextState {
       pub store: Arc<dyn TranscriptStore>,
       pub estimator: Arc<TokenEstimator>,
       pub config: ContextPolicy,
       /// Per-flow locks to serialize concurrent access to the same transcript.
       flow_locks: DashMap<String, Arc<tokio::sync::Mutex<()>>>,
       /// Sender for compression jobs (worker consumes from the other end).
       pub compression_tx: tokio::sync::mpsc::Sender<CompressionJob>,
   }
   ```

2. **`CompressionJob` struct** (defined in `src/context/mod.rs` for now,
   implementation in issue 09):
   ```rust
   #[derive(Debug, Clone)]
   pub struct CompressionJob {
       pub flow_id: String,
       pub turn_range_start: usize,
       pub turn_range_end: usize,
       pub enqueued_at: std::time::Instant,
   }
   ```

3. **`ContextState::new()` constructor**:
   ```rust
   pub async fn new(
       config: ContextPolicy,
       compression_tx: mpsc::Sender<CompressionJob>,
   ) -> anyhow::Result<Self>
   ```
   - Open SQLite store at `config.store_path`
   - Create parent directory for DB file if it doesn't exist
   - Load token estimator from `config.tokenizer_path`
   - Initialize `flow_locks` DashMap
   - Return `Self`

4. **Per-flow locking**:
   ```rust
   pub async fn with_flow_lock<F, R>(&self, flow_id: &str, f: F) -> R
   where
       F: std::future::Future<Output = R>,
   ```
   - Get-or-create a `Mutex` for `flow_id` from `flow_locks`
   - `lock().await` on the mutex
   - Execute `f`
   - Release lock on completion
   - This ensures only one request per flow reconciles/transacts at a time

5. **`trigger_compression()` method**:
   ```rust
   pub fn trigger_compression(&self, job: CompressionJob) -> anyhow::Result<()>
   ```
   - `self.compression_tx.try_send(job)` (non-blocking)
   - If channel full: log warning, return Ok (compression will be retried
     on next request — the trigger is best-effort, not critical-path)
   - If `context_policy.enabled` is false: no-op

6. **Startup scan** — `ContextState::find_flows_needing_compression()`:
   - `store.list_flows_over_threshold(config.compress_threshold)`
   - For each flow: enqueue a `CompressionJob`
   - Called once at startup after `ContextState` is created

7. **`AppState` changes** in `src/gateway/mod.rs`:
   ```rust
   pub struct AppState {
       // ... existing fields ...
       pub context: Option<Arc<ContextState>>,
   }
   ```
   - Use `Option` so the proxy works when `context_policy.enabled = false`
   - If `None`, proxy_handler skips all context compression logic

8. **`main.rs` startup**:
   - After config load, if `config.context_policy.enabled`:
     - Create `mpsc::channel` for compression jobs (buffer = 64)
     - Create `ContextState::new(config.context_policy.clone(), tx).await?`
     - Run startup scan
     - Pass `Some(Arc::new(context_state))` to `AppState`
     - The compression worker (issue 09) will be spawned here too, but
       for now just hold the `rx` (it's fine to drop it — compression
       won't work until issue 09 adds the consumer)
   - If disabled: pass `None`

9. **`AppState` builder** — update `create_router()` or wherever `AppState`
   is constructed to accept the optional `ContextState`.

10. **Graceful degradation**:
    - If `ContextState::new()` fails (e.g., SQLite can't open): log error,
      proceed with `context = None` (proxy works without compression)
    - If store operations fail during reconcile: reconcile returns error,
      proxy_handler forwards original body (fail-open — issue 07)

## Verification

```bash
cargo check 2>&1 | tail -5
# Verify proxy still starts and serves with context_policy.enabled = false:
cargo test --lib gateway 2>&1 | tail -10
# Verify ContextState initializes with enabled = true and a temp DB:
cargo test --lib context_state 2>&1 | tail -5
```

## Tests

- `test_context_state_disabled` — `enabled = false` → `context` is `None`
- `test_context_state_creates_db` — `enabled = true` → DB file created at
  `store_path`
- `test_flow_lock_serializes` — two concurrent calls to `with_flow_lock`
  for the same flow execute sequentially
- `test_flow_lock_concurrent_diff_flows` — two concurrent calls for different
  flows execute in parallel
