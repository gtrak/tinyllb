//! Persistent transcript store backed by SQLite via `sqlx`.
//!
//! Handles CRUD for segments and transcript metadata. Uses WAL mode for
//! concurrent reads during writes. Survives proxy restarts via disk-backed
//! SQLite with embedded migrations.

use crate::context::segment::{Segment, SegmentKind, Transcript};
use anyhow::Context;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};

/// Per-flow transcript metadata stored in `transcript_meta`.
///
/// Provides quick-lookup aggregates so the compression scheduler can decide
/// which flows are overdue without loading full transcripts.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TranscriptMeta {
    /// Flow identifier (primary key).
    pub flow_id: String,
    /// Number of turns in the Head segment.
    pub head_turns: i32,
    /// Number of turns in the Live segment.
    pub live_turns: i32,
    /// Total number of Compressed segments.
    pub compressed_count: i32,
    /// Turn index of the most recent compression point.
    pub last_compressed_turn: i32,
    /// Sum of `est_tokens` across all segments.
    pub total_est_tokens: i32,
    /// Sum of `raw_est_tokens` across all segments.
    pub total_raw_est_tokens: i32,
    /// ISO 8601 timestamp of last update.
    pub updated_at: String,
}

/// Abstract transcript persistence layer.
///
/// Implementations may be in-memory (for tests), SQLite-backed, or any
/// other durable store. The trait is `Send + Sync` so a single instance
/// can be shared across request handlers.
#[async_trait::async_trait]
pub trait TranscriptStore: Send + Sync {
    /// Load all segments for a flow, ordered by `segment_idx`.
    ///
    /// Returns an empty `Transcript` if the flow has no segments.
    async fn load_transcript(&self, flow_id: &str) -> anyhow::Result<Transcript>;

    /// Persist a segment (insert or replace).
    async fn save_segment(&self, seg: &Segment) -> anyhow::Result<()>;

    /// Remove a segment by index.
    async fn delete_segment(&self, flow_id: &str, segment_idx: i32) -> anyhow::Result<()>;

    /// Replace the Live segment with new messages.
    ///
    /// Deletes any existing Live segment and inserts a new one with the
    /// next sequential index.
    async fn update_live_segment(
        &self,
        flow_id: &str,
        messages: &[Value],
        est_tokens: i32,
        raw_est_tokens: i32,
    ) -> anyhow::Result<()>;

    /// Get metadata for a flow (returns `None` if absent).
    async fn get_meta(&self, flow_id: &str) -> anyhow::Result<Option<TranscriptMeta>>;

    /// Insert or replace metadata for a flow.
    async fn upsert_meta(&self, meta: &TranscriptMeta) -> anyhow::Result<()>;

    /// List flows whose estimated token count exceeds `threshold`.
    async fn list_flows_over_threshold(&self, threshold: usize) -> anyhow::Result<Vec<String>>;

    /// Delete all segments and metadata for a flow.
    async fn delete_transcript(&self, flow_id: &str) -> anyhow::Result<()>;

    /// List metadata for all flows, ordered by flow_id.
    async fn list_all_meta(&self) -> anyhow::Result<Vec<TranscriptMeta>>;
}

/// SQLite-backed implementation of `TranscriptStore`.
pub struct SqliteStore {
    pool: SqlitePool,
}

impl SqliteStore {
    /// Open a new (or existing) SQLite database at `path`.
    ///
    /// Use `":memory:"` for an ephemeral in-memory database (tests).
    /// Any other path is opened with read-write-create semantics.
    ///
    /// WAL mode is enabled for file-backed databases. Migrations are
    /// embedded and applied on open.
    pub async fn open(path: &str) -> anyhow::Result<Self> {
        let conn_str = if path == ":memory:" {
            "sqlite::memory:".to_string()
        } else {
            // rwc = read-write-create; creates file if missing.
            format!("sqlite://{}?mode=rwc", path)
        };

        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect(&conn_str)
            .await
            .context("failed to create SQLite pool")?;

        // WAL mode — not supported by in-memory databases, so we
        // intentionally ignore this error for `:memory:`.
        let _ = sqlx::query("PRAGMA journal_mode = WAL;")
            .execute(&pool)
            .await;

        // Busy timeout: 5 seconds for concurrent access.
        sqlx::query("PRAGMA busy_timeout = 5000;")
            .execute(&pool)
            .await
            .context("failed to set busy_timeout")?;

        // Run embedded migrations.
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("failed to run migrations")?;

        Ok(Self { pool })
    }
}

fn kind_to_str(kind: &SegmentKind) -> &'static str {
    match kind {
        SegmentKind::Head => "head",
        SegmentKind::Compressed => "compressed",
        SegmentKind::Live => "live",
    }
}

fn str_to_kind(s: &str) -> SegmentKind {
    match s {
        "head" => SegmentKind::Head,
        "compressed" => SegmentKind::Compressed,
        "live" => SegmentKind::Live,
        _ => panic!("unknown segment kind: {s}"),
    }
}

fn segment_to_row(seg: &Segment) -> (String, i32, &'static str, String, Option<String>, i32, i32, i32, i32, String) {
    (
        seg.flow_id.clone(),
        seg.segment_idx,
        kind_to_str(&seg.kind),
        serde_json::to_string(&seg.raw_messages).expect("serialize raw_messages"),
        seg.summary_message
            .as_ref()
            .map(|v| serde_json::to_string(v).expect("serialize summary_message")),
        seg.msg_start_idx,
        seg.msg_end_idx,
        seg.est_tokens,
        seg.raw_est_tokens,
        seg.created_at.to_rfc3339(),
    )
}

fn row_to_segment(r: &sqlx::sqlite::SqliteRow) -> Segment {
    let flow_id: String = r.get(0);
    let segment_idx: i32 = r.get(1);
    let kind: String = r.get(2);
    let raw_messages: String = r.get(3);
    let summary_msg: Option<String> = r.get(4);
    let msg_start_idx: i32 = r.get(5);
    let msg_end_idx: i32 = r.get(6);
    let est_tokens: i32 = r.get(7);
    let raw_est_tokens: i32 = r.get(8);
    let created_at: String = r.get(9);
    Segment {
        flow_id,
        segment_idx,
        kind: str_to_kind(&kind),
        raw_messages: serde_json::from_str(&raw_messages).expect("deserialize raw_messages"),
        summary_message: summary_msg
            .as_ref()
            .map(|s| serde_json::from_str(s).expect("deserialize summary_message")),
        msg_start_idx,
        msg_end_idx,
        est_tokens,
        raw_est_tokens,
        created_at: DateTime::parse_from_rfc3339(&created_at)
            .expect("parse created_at as RFC3339")
            .with_timezone(&Utc),
    }
}

#[async_trait::async_trait]
impl TranscriptStore for SqliteStore {
    async fn load_transcript(&self, flow_id: &str) -> anyhow::Result<Transcript> {
        let rows = sqlx::query(
            "SELECT flow_id, segment_idx, kind, raw_messages, summary_msg, \
             msg_start_idx, msg_end_idx, est_tokens, raw_est_tokens, created_at \
             FROM segments \
             WHERE flow_id = ? \
             ORDER BY segment_idx"
        )
        .bind(flow_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to load segments")?;

        let segments: Vec<Segment> = rows
            .iter()
            .map(row_to_segment)
            .collect();

        Ok(Transcript {
            flow_id: flow_id.to_string(),
            segments,
        })
    }

    async fn save_segment(&self, seg: &Segment) -> anyhow::Result<()> {
        let (flow_id, segment_idx, kind, raw_messages, summary_msg,
             msg_start_idx, msg_end_idx, est_tokens, raw_est_tokens, created_at) =
            segment_to_row(seg);

        sqlx::query(
            "INSERT OR REPLACE INTO segments \
             (flow_id, segment_idx, kind, raw_messages, summary_msg, \
              msg_start_idx, msg_end_idx, est_tokens, raw_est_tokens, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(flow_id)
        .bind(segment_idx)
        .bind(kind)
        .bind(raw_messages)
        .bind(summary_msg)
        .bind(msg_start_idx)
        .bind(msg_end_idx)
        .bind(est_tokens)
        .bind(raw_est_tokens)
        .bind(created_at)
        .execute(&self.pool)
        .await
        .context("failed to save segment")?;

        Ok(())
    }

    async fn delete_segment(&self, flow_id: &str, segment_idx: i32) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM segments WHERE flow_id = ? AND segment_idx = ?")
            .bind(flow_id)
            .bind(segment_idx)
            .execute(&self.pool)
            .await
            .context("failed to delete segment")?;

        Ok(())
    }

    async fn update_live_segment(
        &self,
        flow_id: &str,
        messages: &[Value],
        est_tokens: i32,
        raw_est_tokens: i32,
    ) -> anyhow::Result<()> {
        // Delete any existing Live segment for this flow.
        sqlx::query("DELETE FROM segments WHERE flow_id = ? AND kind = 'live'")
            .bind(flow_id)
            .execute(&self.pool)
            .await
            .context("failed to delete existing live segment")?;

        // Compute new segment_idx: one past the max index of non-live segments
        // for this flow. If no other segments exist, use 0.
        let new_idx: i32 = sqlx::query_scalar::<_, i32>(
            "SELECT COALESCE(MAX(segment_idx), -1) + 1 \
             FROM segments WHERE flow_id = ? AND kind != 'live'"
        )
        .bind(flow_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to compute live segment index")?;

        let created_at = Utc::now();
        let msg_start_idx: i32 = 0;
        let msg_end_idx: i32 = messages.len() as i32;

        let raw_messages = serde_json::to_string(messages).expect("serialize messages");

        sqlx::query(
            "INSERT OR REPLACE INTO segments \
             (flow_id, segment_idx, kind, raw_messages, summary_msg, \
              msg_start_idx, msg_end_idx, est_tokens, raw_est_tokens, created_at) \
             VALUES (?, ?, 'live', ?, NULL, ?, ?, ?, ?, ?)"
        )
        .bind(flow_id)
        .bind(new_idx)
        .bind(raw_messages)
        .bind(msg_start_idx)
        .bind(msg_end_idx)
        .bind(est_tokens)
        .bind(raw_est_tokens)
        .bind(created_at.to_rfc3339())
        .execute(&self.pool)
        .await
        .context("failed to insert live segment")?;

        Ok(())
    }

    async fn get_meta(&self, flow_id: &str) -> anyhow::Result<Option<TranscriptMeta>> {
        let row = sqlx::query(
            "SELECT flow_id, head_turns, live_turns, compressed_count, \
             last_compressed_turn, total_est_tokens, total_raw_est_tokens, updated_at \
             FROM transcript_meta WHERE flow_id = ?"
        )
        .bind(flow_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get transcript meta")?;

        let meta = row.map(|r| TranscriptMeta {
            flow_id: r.get(0),
            head_turns: r.get(1),
            live_turns: r.get(2),
            compressed_count: r.get(3),
            last_compressed_turn: r.get(4),
            total_est_tokens: r.get(5),
            total_raw_est_tokens: r.get(6),
            updated_at: r.get(7),
        });

        Ok(meta)
    }

    async fn upsert_meta(&self, meta: &TranscriptMeta) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO transcript_meta \
             (flow_id, head_turns, live_turns, compressed_count, \
              last_compressed_turn, total_est_tokens, total_raw_est_tokens, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(&meta.flow_id)
        .bind(meta.head_turns)
        .bind(meta.live_turns)
        .bind(meta.compressed_count)
        .bind(meta.last_compressed_turn)
        .bind(meta.total_est_tokens)
        .bind(meta.total_raw_est_tokens)
        .bind(&meta.updated_at)
        .execute(&self.pool)
        .await
        .context("failed to upsert transcript meta")?;

        Ok(())
    }

    async fn list_flows_over_threshold(&self, threshold: usize) -> anyhow::Result<Vec<String>> {
        let threshold_i64 = threshold as i64;

        let rows = sqlx::query_scalar::<_, String>(
            "SELECT flow_id FROM transcript_meta WHERE total_est_tokens > ?"
        )
        .bind(threshold_i64)
        .fetch_all(&self.pool)
        .await
        .context("failed to list flows over threshold")?;

        Ok(rows)
    }

    async fn delete_transcript(&self, flow_id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM segments WHERE flow_id = ?")
            .bind(flow_id)
            .execute(&self.pool)
            .await
            .context("failed to delete segments")?;

        sqlx::query("DELETE FROM transcript_meta WHERE flow_id = ?")
            .bind(flow_id)
            .execute(&self.pool)
            .await
            .context("failed to delete transcript meta")?;

        Ok(())
    }

    async fn list_all_meta(&self) -> anyhow::Result<Vec<TranscriptMeta>> {
        let rows = sqlx::query(
            "SELECT flow_id, head_turns, live_turns, compressed_count, \
             last_compressed_turn, total_est_tokens, total_raw_est_tokens, updated_at \
             FROM transcript_meta ORDER BY flow_id"
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to list all transcript meta")?;

        let metas: Vec<TranscriptMeta> = rows
            .iter()
            .map(|r| TranscriptMeta {
                flow_id: r.get(0),
                head_turns: r.get(1),
                live_turns: r.get(2),
                compressed_count: r.get(3),
                last_compressed_turn: r.get(4),
                total_est_tokens: r.get(5),
                total_raw_est_tokens: r.get(6),
                updated_at: r.get(7),
            })
            .collect();

        Ok(metas)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn new_store() -> SqliteStore {
        SqliteStore::open(":memory:").await.expect("open in-memory store")
    }

    fn make_head_segment(flow_id: &str) -> Segment {
        Segment {
            flow_id: flow_id.to_string(),
            segment_idx: 0,
            kind: SegmentKind::Head,
            raw_messages: serde_json::json!([
                { "role": "system", "content": "you are helpful" },
                { "role": "user", "content": "hello" },
            ])
            .as_array()
            .unwrap()
            .to_vec(),
            summary_message: None,
            msg_start_idx: 0,
            msg_end_idx: 2,
            est_tokens: 100,
            raw_est_tokens: 100,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_save_and_load_segment() {
        let store = new_store().await;
        let flow_id = "test-flow-1";

        let seg = make_head_segment(flow_id);
        store.save_segment(&seg).await.expect("save segment");

        let transcript = store.load_transcript(flow_id).await.expect("load transcript");
        assert_eq!(transcript.flow_id, flow_id);
        assert_eq!(transcript.segments.len(), 1);

        let loaded = &transcript.segments[0];
        assert_eq!(loaded.kind, SegmentKind::Head);
        assert_eq!(loaded.raw_messages.len(), 2);
        assert_eq!(loaded.est_tokens, 100);
        // created_at round-trips (compare as RFC3339 to avoid sub-second drift)
        assert_eq!(
            loaded.created_at.to_rfc3339(),
            seg.created_at.to_rfc3339()
        );
    }

    #[tokio::test]
    async fn test_update_live_segment() {
        let store = new_store().await;
        let flow_id = "test-flow-2";

        // Initial live segment with 2 messages.
        let msgs_2: Vec<Value> = serde_json::json!([
            { "role": "user", "content": "first" },
            { "role": "assistant", "content": "ok" },
        ])
        .as_array()
        .unwrap()
        .to_vec();

        store
            .update_live_segment(flow_id, &msgs_2, 50, 50)
            .await
            .expect("update live first time");

        // Verify one live segment with 2 messages.
        let t1 = store.load_transcript(flow_id).await.expect("load 1");
        assert_eq!(t1.segments.len(), 1);
        assert_eq!(t1.segments[0].kind, SegmentKind::Live);
        assert_eq!(t1.segments[0].raw_messages.len(), 2);

        // Update with 4 messages.
        let msgs_4: Vec<Value> = serde_json::json!([
            { "role": "user", "content": "first" },
            { "role": "assistant", "content": "ok" },
            { "role": "user", "content": "second" },
            { "role": "assistant", "content": "ok again" },
        ])
        .as_array()
        .unwrap()
        .to_vec();

        store
            .update_live_segment(flow_id, &msgs_4, 100, 100)
            .await
            .expect("update live second time");

        // Verify replaced: still exactly one live segment, now with 4 messages.
        let t2 = store.load_transcript(flow_id).await.expect("load 2");
        assert_eq!(t2.segments.len(), 1);
        assert_eq!(t2.segments[0].kind, SegmentKind::Live);
        assert_eq!(t2.segments[0].raw_messages.len(), 4);
    }

    #[tokio::test]
    async fn test_delete_transcript() {
        let store = new_store().await;
        let flow_id = "test-flow-3";

        // Create a segment and meta.
        let seg = make_head_segment(flow_id);
        store.save_segment(&seg).await.expect("save segment");

        let meta = TranscriptMeta {
            flow_id: flow_id.to_string(),
            head_turns: 1,
            live_turns: 0,
            compressed_count: 0,
            last_compressed_turn: 0,
            total_est_tokens: 100,
            total_raw_est_tokens: 100,
            updated_at: Utc::now().to_rfc3339(),
        };
        store.upsert_meta(&meta).await.expect("upsert meta");

        // Verify they exist.
        let t_before = store.load_transcript(flow_id).await.expect("load before");
        assert!(!t_before.segments.is_empty());
        assert!(store.get_meta(flow_id).await.expect("get meta").is_some());

        // Delete.
        store.delete_transcript(flow_id).await.expect("delete transcript");

        // Verify gone.
        let t_after = store.load_transcript(flow_id).await.expect("load after");
        assert!(t_after.segments.is_empty());
        assert!(store.get_meta(flow_id).await.expect("get meta after").is_none());
    }

    #[tokio::test]
    async fn test_list_flows_over_threshold() {
        let store = new_store().await;

        // Flow A: 50 tokens (below threshold of 100).
        store.upsert_meta(&TranscriptMeta {
            flow_id: "flow-a".to_string(),
            total_est_tokens: 50,
            updated_at: Utc::now().to_rfc3339(),
            ..Default::default()
        })
        .await
        .expect("upsert A");

        // Flow B: 150 tokens (above threshold).
        store.upsert_meta(&TranscriptMeta {
            flow_id: "flow-b".to_string(),
            total_est_tokens: 150,
            updated_at: Utc::now().to_rfc3339(),
            ..Default::default()
        })
        .await
        .expect("upsert B");

        // Flow C: 200 tokens (above threshold).
        store.upsert_meta(&TranscriptMeta {
            flow_id: "flow-c".to_string(),
            total_est_tokens: 200,
            updated_at: Utc::now().to_rfc3339(),
            ..Default::default()
        })
        .await
        .expect("upsert C");

        let flows = store
            .list_flows_over_threshold(100)
            .await
            .expect("list over threshold");

        assert_eq!(flows.len(), 2);
        assert!(flows.contains(&"flow-b".to_string()));
        assert!(flows.contains(&"flow-c".to_string()));
        assert!(!flows.contains(&"flow-a".to_string()));
    }

    #[tokio::test]
    async fn test_persistence() {
        let tmp_dir = std::env::temp_dir();
        let db_path = tmp_dir.join(format!("llm-qdisc-test-persist-{}.db", chrono::Utc::now().timestamp_millis()));

        // Open, write, close.
        {
            let store = SqliteStore::open(db_path.to_str().unwrap())
                .await
                .expect("open file store");

            let seg = make_head_segment("persist-flow");
            store.save_segment(&seg).await.expect("save segment");

            // Drop the store to close connections.
            drop(store);
        }

        // Reopen and verify.
        let store2 = SqliteStore::open(db_path.to_str().unwrap())
            .await
            .expect("reopen file store");

        let t = store2.load_transcript("persist-flow").await.expect("load transcript");
        assert_eq!(t.segments.len(), 1);
        assert_eq!(t.segments[0].kind, SegmentKind::Head);
        assert_eq!(t.segments[0].est_tokens, 100);

        // Clean up.
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn test_upsert_meta() {
        let store = new_store().await;

        // First upsert.
        let meta1 = TranscriptMeta {
            flow_id: "upsert-flow".to_string(),
            head_turns: 5,
            live_turns: 3,
            compressed_count: 2,
            last_compressed_turn: 10,
            total_est_tokens: 500,
            total_raw_est_tokens: 1000,
            updated_at: "2025-01-01T00:00:00Z".to_string(),
        };
        store.upsert_meta(&meta1).await.expect("upsert first");

        let fetched = store.get_meta("upsert-flow").await.expect("get meta");
        let fetched = fetched.expect("meta should exist");
        assert_eq!(fetched.head_turns, 5);
        assert_eq!(fetched.total_est_tokens, 500);

        // Second upsert with new values.
        let meta2 = TranscriptMeta {
            flow_id: "upsert-flow".to_string(),
            head_turns: 10,
            live_turns: 6,
            compressed_count: 5,
            last_compressed_turn: 20,
            total_est_tokens: 1000,
            total_raw_est_tokens: 2000,
            updated_at: "2025-06-01T00:00:00Z".to_string(),
        };
        store.upsert_meta(&meta2).await.expect("upsert second");

        let fetched2 = store.get_meta("upsert-flow").await.expect("get meta 2");
        let fetched2 = fetched2.expect("meta should still exist");
        assert_eq!(fetched2.head_turns, 10);
        assert_eq!(fetched2.total_est_tokens, 1000);
        assert_eq!(fetched2.updated_at, "2025-06-01T00:00:00Z");
    }

    #[tokio::test]
    async fn test_list_all_meta() {
        let store = new_store().await;

        // Insert 3 flows with distinct metadata.
        store.upsert_meta(&TranscriptMeta {
            flow_id: "alpha".to_string(),
            head_turns: 2,
            live_turns: 3,
            compressed_count: 1,
            last_compressed_turn: 5,
            total_est_tokens: 300,
            total_raw_est_tokens: 500,
            updated_at: "2025-01-01T00:00:00Z".to_string(),
        })
        .await
        .expect("upsert alpha");

        store.upsert_meta(&TranscriptMeta {
            flow_id: "beta".to_string(),
            head_turns: 4,
            live_turns: 5,
            compressed_count: 2,
            last_compressed_turn: 10,
            total_est_tokens: 800,
            total_raw_est_tokens: 1200,
            updated_at: "2025-02-01T00:00:00Z".to_string(),
        })
        .await
        .expect("upsert beta");

        store.upsert_meta(&TranscriptMeta {
            flow_id: "gamma".to_string(),
            head_turns: 1,
            live_turns: 2,
            compressed_count: 0,
            last_compressed_turn: 0,
            total_est_tokens: 150,
            total_raw_est_tokens: 150,
            updated_at: "2025-03-01T00:00:00Z".to_string(),
        })
        .await
        .expect("upsert gamma");

        let metas = store.list_all_meta().await.expect("list all meta");
        assert_eq!(metas.len(), 3);

        // Should be ordered by flow_id alphabetically.
        assert_eq!(metas[0].flow_id, "alpha");
        assert_eq!(metas[0].total_est_tokens, 300);
        assert_eq!(metas[0].total_raw_est_tokens, 500);

        assert_eq!(metas[1].flow_id, "beta");
        assert_eq!(metas[1].total_est_tokens, 800);
        assert_eq!(metas[1].compressed_count, 2);

        assert_eq!(metas[2].flow_id, "gamma");
        assert_eq!(metas[2].total_est_tokens, 150);
        assert_eq!(metas[2].live_turns, 2);
    }
}
