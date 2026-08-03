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
