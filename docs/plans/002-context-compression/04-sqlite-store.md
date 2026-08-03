# Issue 04 — SQLite Store (sqlx)

## Objective

Implement a persistent transcript store backed by SQLite via `sqlx`. The store
handles CRUD for segments and transcript metadata, survives proxy restarts, and
supports concurrent reads during writes (WAL mode).

## Files

| File | Change |
|------|--------|
| `migrations/0001_transcripts.sql` | New — schema DDL |
| `src/context/store.rs` | New — `TranscriptStore` trait + `SqliteStore` impl |
| `src/context/mod.rs` | Add `pub mod store;` |

## Prerequisites

- Issue 01 (deps + config — `store_path`)
- Issue 03 (segment model — `Segment`, `SegmentKind`)

## Design decisions

- **sqlx runtime queries** (`sqlx::query()`, not `sqlx::query!` macros). This
  avoids the need for a live DB or offline `.sqlx` data at compile time.
  Queries are simple CRUD — runtime checking is sufficient.
- **WAL mode** for concurrent reads during writes (the proxy forwards requests
  on the read path while the compression worker writes).
- **`sqlx::SqlitePool`** with `max_connections = 4` (enough for concurrent
  reads + 1 writer; SQLite serializes writes anyway).
- **`migrate!` macro** embeds migration SQL at compile time from
  `migrations/` directory.

## Steps

1. **Migration** — create `migrations/0001_transcripts.sql`:
   ```sql
   CREATE TABLE IF NOT EXISTS segments (
       flow_id       TEXT    NOT NULL,
       segment_idx   INTEGER NOT NULL,
       kind          TEXT    NOT NULL CHECK(kind IN ('head', 'compressed', 'live')),
       raw_messages  TEXT    NOT NULL,    -- JSON array of message objects
       summary_msg   TEXT,                 -- JSON object (compressed only)
       msg_start_idx INTEGER NOT NULL,
       msg_end_idx   INTEGER NOT NULL,
       est_tokens    INTEGER NOT NULL,
       raw_est_tokens INTEGER NOT NULL,
       created_at    TEXT    NOT NULL,
       PRIMARY KEY (flow_id, segment_idx)
   );

   CREATE INDEX IF NOT EXISTS idx_segments_flow
       ON segments(flow_id, segment_idx);

   CREATE TABLE IF NOT EXISTS transcript_meta (
       flow_id             TEXT PRIMARY KEY,
       head_turns          INTEGER NOT NULL DEFAULT 0,
       live_turns          INTEGER NOT NULL DEFAULT 0,
       compressed_count    INTEGER NOT NULL DEFAULT 0,
       last_compressed_turn INTEGER NOT NULL DEFAULT 0,
       total_est_tokens    INTEGER NOT NULL DEFAULT 0,
       total_raw_est_tokens INTEGER NOT NULL DEFAULT 0,
       updated_at          TEXT NOT NULL
   );

   CREATE INDEX IF NOT EXISTS idx_meta_overdue
       ON transcript_meta(total_est_tokens) WHERE total_est_tokens > 0;
   ```

2. **`TranscriptStore` trait** in `src/context/store.rs`:
   ```rust
   #[async_trait::async_trait]
   pub trait TranscriptStore: Send + Sync {
       async fn load_transcript(&self, flow_id: &str) -> anyhow::Result<Transcript>;
       async fn save_segment(&self, seg: &Segment) -> anyhow::Result<()>;
       async fn delete_segment(&self, flow_id: &str, segment_idx: i32) -> anyhow::Result<()>;
       async fn update_live_segment(&self, flow_id: &str, messages: &[Value], est_tokens: i32, raw_est_tokens: i32) -> anyhow::Result<()>;
       async fn get_meta(&self, flow_id: &str) -> anyhow::Result<Option<TranscriptMeta>>;
       async fn upsert_meta(&self, meta: &TranscriptMeta) -> anyhow::Result<()>;
       async fn list_flows_over_threshold(&self, threshold: usize) -> anyhow::Result<Vec<String>>;
       async fn delete_transcript(&self, flow_id: &str) -> anyhow::Result<()>;
   }
   ```
   Add `async-trait` to Cargo.toml deps if not already present.

3. **`TranscriptMeta` struct** (mirror of `transcript_meta` table):
   ```rust
   #[derive(Debug, Clone, Default, serde::Serialize)]
   pub struct TranscriptMeta {
       pub flow_id: String,
       pub head_turns: i32,
       pub live_turns: i32,
       pub compressed_count: i32,
       pub last_compressed_turn: i32,
       pub total_est_tokens: i32,
       pub total_raw_est_tokens: i32,
       pub updated_at: String,
   }
   ```

4. **`SqliteStore` implementation**:
   ```rust
   pub struct SqliteStore {
       pool: SqlitePool,
   }
   ```
   Constructor: `SqliteStore::open(path: &str) -> anyhow::Result<Self>`:
   - Build connection string: `sqlite://{path}?mode=rwc`
   - Create pool: `SqlitePoolOptions::new().max_connections(4).connect(&conn_str).await`
   - Run `PRAGMA journal_mode = WAL;`
   - Run `PRAGMA busy_timeout = 5000;`
   - Run `sqlx::migrate!("./migrations")` to apply schema
   - Return `Self { pool }`

5. **`load_transcript`**:
   - Query all segments for `flow_id` ordered by `segment_idx`
   - Deserialize `raw_messages` and `summary_msg` from JSON text
   - Build `Transcript { flow_id, segments }`

6. **`save_segment`**:
   - `INSERT OR REPLACE INTO segments (...) VALUES (...)`
   - Serialize `raw_messages` and `summary_msg` to JSON strings
   - Use `sqlx::query()` with bind params

7. **`update_live_segment`**:
   - This is the most common write path (appending new turns to Live)
   - `INSERT OR REPLACE` the live segment (segment_idx for live is always
     the highest idx, OR use a fixed convention: live segment has
     `segment_idx = -1` to distinguish from compressed segments)

   **Convention**: `segment_idx` for Head = 0, Compressed segments = 1, 2, ...,
   n, Live = n+1 (one more than the last compressed). On `update_live_segment`,
   we `DELETE` the old live segment and `INSERT` the new one. This avoids
   in-place mutation of segment_idx.

   Alternative simpler approach: Live always has `segment_idx = 0` if no
   compressed segments exist, or `segment_idx = max_compressed_idx + 1`.
   The `update_live_segment` method handles this:
   - Find current max `segment_idx` for Compressed segments
   - Delete any existing Live segment (WHERE `kind = 'live'`)
   - Insert new Live segment with appropriate idx

8. **`delete_transcript`**:
   - `DELETE FROM segments WHERE flow_id = ?`
   - `DELETE FROM transcript_meta WHERE flow_id = ?`

9. **`list_flows_over_threshold`**:
   - `SELECT flow_id FROM transcript_meta WHERE total_est_tokens > ?`
   - Used on startup to find flows needing compression

10. **Error handling**: map `sqlx::Error` to `anyhow::Error` with context.
    Log errors at `tracing::warn` level.

11. **Tests** — use in-memory SQLite (`sqlite::memory:`):
    - `test_save_and_load_segment` — save a Head segment, load transcript,
      verify fields match
    - `test_update_live_segment` — create live, update with more messages,
      verify replacement
    - `test_delete_transcript` — create segments + meta, delete, verify gone
    - `test_list_flows_over_threshold` — create 3 flows with different token
      counts, query with threshold, verify correct subset returned
    - `test_persistence` — save to a temp file, close pool, reopen, verify
      data intact

## Verification

```bash
cargo test --lib store 2>&1 | tail -10
# Verify migration applies cleanly on a fresh DB:
cargo test --lib test_persistence 2>&1 | tail -5
# Verify WAL mode:
# (run a test that opens the DB, then check journal_mode)
```

## Notes

- Add `async-trait = "0.1"` to Cargo.toml if not already a dependency
- `sqlx::migrate!` macro requires the `migrations/` directory to be at
  `CARGO_MANIFEST_DIR/migrations/` at compile time
- All timestamps use `chrono::Utc::now()` formatted as ISO 8601 strings
- Add `chrono = { version = "0.4", features = ["serde"] }` to Cargo.toml
  if not already present
