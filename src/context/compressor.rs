//! Background compression worker that consumes `CompressionJob` items from an
//! mpsc channel, calls the vLLM backend for summarization, stores compressed
//! segments, and shrinks the Live segment.
//!
//! The sidecar request goes directly to the backend (vLLM), NOT through the
//! proxy itself. This avoids self-referential HTTP. Future work can route
//! through the proxy if needed.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use chrono::Utc;
use serde_json::json;
use tokio::sync::mpsc;
use url::Url;

use crate::context::segment::{find_turn_boundaries, Segment, SegmentKind, Transcript};
use crate::context::store::TranscriptMeta;
use crate::context::{CompressionJob, ContextState};

/// Background worker that drains `CompressionJob` messages and processes them
/// sequentially (one at a time) to avoid competing for vLLM slots.
pub struct CompressionWorker {
    rx: mpsc::Receiver<CompressionJob>,
    ctx: Arc<ContextState>,
    backend_url: Url,
    client: reqwest::Client,
}

impl CompressionWorker {
    pub fn new(
        rx: mpsc::Receiver<CompressionJob>,
        ctx: Arc<ContextState>,
        backend_url: Url,
        client: reqwest::Client,
    ) -> Self {
        Self {
            rx,
            ctx,
            backend_url,
            client,
        }
    }

    /// Main loop: consume jobs until the channel is closed.
    pub async fn run(mut self) {
        tracing::info!("context compression worker started");
        while let Some(job) = self.rx.recv().await {
            if let Err(e) = self.process_job(&job).await {
                tracing::warn!(
                    flow_id = %job.flow_id,
                    error = %e,
                    "compression job failed"
                );
            }
        }
        tracing::info!("context compression worker stopped");
    }

    async fn process_job(&self, job: &CompressionJob) -> anyhow::Result<()> {
        self.ctx
            .with_flow_lock(&job.flow_id, async {
                self.process_job_locked(job).await
            })
            .await
    }

    async fn process_job_locked(&self, job: &CompressionJob) -> anyhow::Result<()> {
        // Load the current transcript from the store.
        let transcript = self
            .ctx
            .store
            .load_transcript(&job.flow_id)
            .await
            .context("load transcript")?;

        let live_segment = transcript
            .live_segment()
            .ok_or_else(|| anyhow::anyhow!("no live segment found for {}", job.flow_id))?;

        let live_raw = &live_segment.raw_messages;
        let live_turns = find_turn_boundaries(live_raw).len();

        // Determine the turn range to compress.
        let (turn_start, turn_end) = if job.turn_range_end == 0 {
            // Startup scan placeholder — recompute from the live segment.
            let compressible = live_turns.saturating_sub(self.ctx.config.live_keep_turns);
            if compressible == 0 {
                tracing::info!(
                    flow_id = %job.flow_id,
                    live_turns,
                    "nothing to compress (live segment <= live_keep_turns)"
                );
                return Ok(());
            }
            let end = compressible.min(self.ctx.config.compress_chunk_turns);
            (0, end)
        } else {
            (job.turn_range_start, job.turn_range_end)
        };

        // Map turn indices to message indices via turn boundaries.
        let boundaries = find_turn_boundaries(live_raw);
        let msg_start = boundaries[turn_start];
        let msg_end = boundaries
            .get(turn_end)
            .copied()
            .unwrap_or(live_raw.len());

        let messages_to_compress = live_raw[msg_start..msg_end].to_vec();
        if messages_to_compress.is_empty() {
            return Ok(());
        }

        // Build the summarization prompt.
        let prompt = self
            .ctx
            .prompt_builder
            .build(&messages_to_compress, turn_start, turn_end);

        // Call the sidecar (direct to vLLM backend).
        let summary_text = self
            .call_sidecar_with_retry(&prompt, &job.flow_id)
            .await?;

        // Create the Compressed segment.
        let summary_message = json!({
            "role": "system",
            "content": summary_text,
        });

        let summary_tokens = self.ctx.estimator.estimate_text(&summary_text) as i32;
        let raw_tokens = self.ctx.estimator.estimate_messages(&messages_to_compress) as i32;

        // Determine the next compressed segment index.
        let next_idx = transcript
            .compressed_segments()
            .map(|s| s.segment_idx)
            .max()
            .map(|m| m + 1)
            .unwrap_or(if transcript.head_segment().is_some() {
                1
            } else {
                0
            });

        let compressed_segment = Segment {
            flow_id: job.flow_id.clone(),
            segment_idx: next_idx,
            kind: SegmentKind::Compressed,
            raw_messages: messages_to_compress.clone(),
            summary_message: Some(summary_message),
            msg_start_idx: live_segment.msg_start_idx + msg_start as i32,
            msg_end_idx: live_segment.msg_start_idx + msg_end as i32,
            est_tokens: summary_tokens,
            raw_est_tokens: raw_tokens,
            created_at: Utc::now(),
        };

        // Update the Live segment by removing the compressed messages.
        let new_live_messages = live_raw[msg_end..].to_vec();
        let new_live_est = self.ctx.estimator.estimate_messages(&new_live_messages) as i32;
        let new_live_raw_est = new_live_est; // Live segments always forward raw

        self.ctx
            .store
            .update_live_segment(
                &job.flow_id,
                &new_live_messages,
                new_live_est,
                new_live_raw_est,
            )
            .await
            .context("update live segment")?;

        // Persist the Compressed segment.
        self.ctx
            .store
            .save_segment(&compressed_segment)
            .await
            .context("save compressed segment")?;

        // Update metadata.
        let meta = self.rebuild_meta(&transcript, summary_tokens, raw_tokens, &new_live_messages);
        self.ctx
            .store
            .upsert_meta(&meta)
            .await
            .context("upsert meta")?;

        tracing::info!(
            flow_id = %job.flow_id,
            compressed_turns = turn_end - turn_start,
            raw_tokens,
            summary_tokens,
            "compression job completed"
        );
        Ok(())
    }

    /// Rebuild TranscriptMeta by re-loading the transcript after the update.
    fn rebuild_meta(
        &self,
        _old: &Transcript,
        _summary_tokens: i32,
        _raw_tokens: i32,
        _new_live_messages: &[serde_json::Value],
    ) -> TranscriptMeta {
        // We cannot rebuild the full meta here because the live segment has
        // already been updated in the store (with a new segment_idx).  The
        // safest approach is to compute what we can from the old transcript
        // and the new data, then store it.
        //
        // Head turns = old head turns (unchanged).
        // Live turns = turns in new live messages.
        // Compressed count = old compressed count + 1.
        // Tokens: subtract old live tokens, add summary + new live tokens.

        let live_turns = find_turn_boundaries(_new_live_messages).len() as i32;

        let head_turns = _old
            .head_segment()
            .map(|s| {
                find_turn_boundaries(&s.raw_messages).len() as i32
            })
            .unwrap_or(0);

        let compressed_count = _old.compressed_segments().count() as i32 + 1;

        let old_total_est: i32 = _old.total_est_tokens() as i32;
        let old_total_raw: i32 = _old.total_raw_est_tokens() as i32;

        // Remove the old live segment's contribution and add the new one.
        let old_live = _old.live_segment();
        let old_live_est = old_live.map(|s| s.est_tokens).unwrap_or(0);
        let old_live_raw = old_live.map(|s| s.raw_est_tokens).unwrap_or(0);

        let new_live_est = self.ctx.estimator.estimate_messages(_new_live_messages) as i32;

        let total_est_tokens = old_total_est
            .saturating_sub(old_live_est)
            + _summary_tokens
            + new_live_est;
        let total_raw_est_tokens = old_total_raw
            .saturating_sub(old_live_raw)
            + _raw_tokens
            + new_live_est;

        let last_compressed_turn = find_turn_boundaries(_new_live_messages).len() as i32;

        TranscriptMeta {
            flow_id: _old.flow_id.clone(),
            head_turns,
            live_turns,
            compressed_count,
            last_compressed_turn,
            total_est_tokens,
            total_raw_est_tokens,
            updated_at: Utc::now().to_rfc3339(),
        }
    }

    async fn call_sidecar_with_retry(
        &self,
        prompt: &[serde_json::Value],
        flow_id: &str,
    ) -> anyhow::Result<String> {
        let max_retries = self.ctx.config.compression_retries;
        for attempt in 0..max_retries {
            match self.call_sidecar(prompt, flow_id).await {
                Ok(text) => return Ok(text),
                Err(e) if attempt < max_retries - 1 => {
                    let backoff = Duration::from_secs(1 << attempt);
                    tracing::warn!(
                        attempt,
                        max_retries,
                        error = %e,
                        flow_id,
                        "sidecar call failed, retrying in {:?}",
                        backoff
                    );
                    tokio::time::sleep(backoff).await;
                }
                Err(e) => return Err(e),
            }
        }
        // Unreachable because the last iteration returns Err.
        Err(anyhow::anyhow!("unexpected end of retry loop"))
    }

    /// Call the vLLM backend for a summarization completion.
    ///
    /// Goes directly to the backend URL (not through the proxy) to avoid
    /// self-referential HTTP. The `X-LLM-Internal` and `X-LLM-Flow-ID`
    /// headers are set in case the request is ever routed through the proxy.
    async fn call_sidecar(
        &self,
        prompt: &[serde_json::Value],
        flow_id: &str,
    ) -> anyhow::Result<String> {
        let body = json!({
            "model": "compressor",
            "messages": prompt,
            "max_tokens": self.ctx.config.summary_max_tokens,
            "temperature": 0.3,
            "stream": false,
        });

        let url = self
            .backend_url
            .join("v1/chat/completions")
            .context("join sidecar URL")?;

        let resp = self
            .client
            .post(url)
            .header("X-LLM-Flow-ID", format!("compressor:{}", flow_id))
            .header("X-LLM-Internal", "compressor")
            .json(&body)
            .timeout(self.ctx.config.sidecar_request_timeout)
            .send()
            .await
            .context("send sidecar request")?;

        if !resp.status().is_success() {
            return Err(anyhow::anyhow!(
                "sidecar returned status {}: {}",
                resp.status(),
                resp.text()
                    .await
                    .unwrap_or_else(|_| "<body>".to_string())
            ));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .context("parse sidecar response JSON")?;

        let content = json
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .ok_or_else(|| anyhow::anyhow!("sidecar response missing choices[0].message.content"))?;

        if content.is_empty() {
            return Err(anyhow::anyhow!(
                "sidecar returned empty content"
            ));
        }

        Ok(content.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ContextPolicy;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    /// Build a bare ContextState (not wrapped in Arc) with an in-memory SQLite
    /// store, and return it alongside the receiver. The caller is responsible for
    /// wrapping in Arc before spawning the worker.
    async fn build_test_state_bare(
        config: ContextPolicy,
    ) -> (ContextState, mpsc::Receiver<CompressionJob>) {
        let (tx, rx) = mpsc::channel::<CompressionJob>(16);
        let state = ContextState::new(config, tx).await.expect("new context state");
        (state, rx)
    }

    /// Helper: wrap bare state in Arc and close the channel.
    /// Since the test holds the only Arc, `Arc::get_mut` succeeds.
    fn wrap_state_for_worker(state: ContextState) -> Arc<ContextState> {
        let mut state = Arc::new(state);
        // Close the compression channel so the worker exits after processing.
        // Safe because we're the only Arc holder at this point.
        Arc::get_mut(&mut state)
            .expect("should be the only Arc holder")
            .close_compression_channel();
        state
    }

    /// Build a ContextPolicy with in-memory store and known compression settings.
    fn test_policy() -> ContextPolicy {
        ContextPolicy {
            enabled: true,
            store_path: ":memory:".to_string(),
            live_keep_turns: 2,
            compress_chunk_turns: 4,
            summary_max_tokens: 256,
            compression_retries: 3,
            sidecar_request_timeout: Duration::from_secs(10),
            tokenizer_path: None,
            prompt_template_path: None,
            ..Default::default()
        }
    }

    /// Build a mock vLLM backend that returns canned responses.
    /// Returns the base URL (http://127.0.0.1:PORT) and the AtomicUsize hit counter.
    async fn start_mock_backend(
        response: String,
        fail_times: usize,
    ) -> (Url, Arc<AtomicUsize>) {
        let hit_counter = Arc::new(AtomicUsize::new(0));
        let response = Arc::new(response);

        let counter = Arc::clone(&hit_counter);
        let resp = Arc::clone(&response);
        let ft = fail_times;

        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |_body: axum::extract::Json<serde_json::Value>| {
                let counter = Arc::clone(&counter);
                let resp = Arc::clone(&resp);
                async move {
                    let count = counter.fetch_add(1, Ordering::SeqCst);
                    if count < ft {
                        axum::response::Json(serde_json::json!({
                            "error": { "message": "simulated failure" }
                        }))
                    } else {
                        axum::response::Json(serde_json::from_str(&resp).unwrap())
                    }
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{}", addr);

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Give server a moment to start.
        tokio::time::sleep(Duration::from_millis(50)).await;

        (Url::parse(&base_url).unwrap(), hit_counter)
    }

    #[tokio::test]
    async fn test_worker_processes_job() {
        let policy = test_policy();
        let (state, rx) = build_test_state_bare(policy).await;

        // Seed a Live segment with enough turns.
        let flow_id = "test-flow-1".to_string();
        let msgs: Vec<serde_json::Value> = (0..12)
            .map(|i| {
                json!({
                    "role": if i % 2 == 0 { "user" } else { "assistant" },
                    "content": format!("turn {} message", i),
                })
            })
            .collect();
        let est = state.estimator.estimate_messages(&msgs) as i32;

        state
            .store
            .update_live_segment(&flow_id, &msgs, est, est)
            .await
            .unwrap();

        // Enqueue a job with placeholder turn_range (0,0) — the worker recomputes.
        state
            .compression_tx
            .as_ref()
            .unwrap()
            .send(CompressionJob {
                flow_id: flow_id.clone(),
                turn_range_start: 0,
                turn_range_end: 0,
                enqueued_at: std::time::Instant::now(),
            })
            .await
            .unwrap();

        // Start mock backend that returns a canned summary.
        let canned = serde_json::json!({
            "choices": [{
                "message": { "content": "this is a summary" },
                "index": 0
            }]
        });
        let (backend_url, _counter) =
            start_mock_backend(canned.to_string(), 0).await;

        let state = wrap_state_for_worker(state);
        let worker = CompressionWorker::new(rx, state, backend_url, reqwest::Client::new());

        // Give the worker a timeout to finish.
        let result = tokio::time::timeout(Duration::from_secs(10), worker.run()).await;
        assert!(result.is_ok(), "worker should complete within timeout");
    }

    #[tokio::test]
    async fn test_worker_skips_when_nothing_to_compress() {
        let policy = test_policy();
        let (state, rx) = build_test_state_bare(policy).await;

        // Seed a Live segment with only 2 turns (= live_keep_turns, so nothing compressible).
        let flow_id = "short-flow".to_string();
        let msgs: Vec<serde_json::Value> = vec![
            json!({"role": "user", "content": "hello"}),
            json!({"role": "assistant", "content": "hi"}),
        ];
        let est = state.estimator.estimate_messages(&msgs) as i32;

        state
            .store
            .update_live_segment(&flow_id, &msgs, est, est)
            .await
            .unwrap();

        // Enqueue a job.
        state
            .compression_tx
            .as_ref()
            .unwrap()
            .send(CompressionJob {
                flow_id: flow_id.clone(),
                turn_range_start: 0,
                turn_range_end: 0,
                enqueued_at: std::time::Instant::now(),
            })
            .await
            .unwrap();

        // Start a dummy backend (won't be called).
        let canned = serde_json::json!({
            "choices": [{
                "message": { "content": "should-not-be-called" },
                "index": 0
            }]
        });
        let (backend_url, _counter) =
            start_mock_backend(canned.to_string(), 0).await;

        let state = wrap_state_for_worker(state);
        let worker = CompressionWorker::new(rx, state, backend_url, reqwest::Client::new());

        let result = tokio::time::timeout(Duration::from_secs(10), worker.run()).await;
        assert!(result.is_ok(), "worker should complete within timeout");
    }

    #[tokio::test]
    async fn test_worker_retries_on_failure() {
        let policy = test_policy();
        let (state, rx) = build_test_state_bare(policy).await;

        // Seed a Live segment with enough turns.
        let flow_id = "retry-flow".to_string();
        let msgs: Vec<serde_json::Value> = (0..12)
            .map(|i| {
                json!({
                    "role": if i % 2 == 0 { "user" } else { "assistant" },
                    "content": format!("turn {} message", i),
                })
            })
            .collect();
        let est = state.estimator.estimate_messages(&msgs) as i32;

        state
            .store
            .update_live_segment(&flow_id, &msgs, est, est)
            .await
            .unwrap();

        state
            .compression_tx
            .as_ref()
            .unwrap()
            .send(CompressionJob {
                flow_id: flow_id.clone(),
                turn_range_start: 0,
                turn_range_end: 0,
                enqueued_at: std::time::Instant::now(),
            })
            .await
            .unwrap();

        // Mock backend fails twice then succeeds.
        let canned = serde_json::json!({
            "choices": [{
                "message": { "content": "summary after retries" },
                "index": 0
            }]
        });
        let (backend_url, hit_counter) =
            start_mock_backend(canned.to_string(), 2).await;

        let state = wrap_state_for_worker(state);
        let worker = CompressionWorker::new(rx, state, backend_url, reqwest::Client::new());

        let result = tokio::time::timeout(Duration::from_secs(30), worker.run()).await;
        assert!(result.is_ok(), "worker should complete within timeout");

        // Should have at least 3 hits (2 failures + 1 success).
        assert!(
            hit_counter.load(Ordering::SeqCst) >= 3,
            "expected at least 3 sidecar attempts"
        );
    }
}
