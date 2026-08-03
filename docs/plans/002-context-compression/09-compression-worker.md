# Issue 09 — Background Compression Worker

## Objective

Implement the background tokio task that consumes compression jobs from the
mpsc channel, makes sidecar summarization requests to the vLLM backend,
stores the resulting summary as a new Compressed segment, and shrinks the
Live segment.

This is the "write" path — it mutates the transcript asynchronously while
the proxy continues serving requests on the "read" path (issue 07).

## Files

| File | Change |
|------|--------|
| `src/context/compressor.rs` | New — `CompressionWorker` + sidecar logic |
| `src/context/mod.rs` | Add `pub mod compressor;` |
| `src/main.rs` | Spawn worker task |

## Prerequisites

- Issue 04 (SQLite store)
- Issue 06 (context state — `ContextState`, `CompressionJob`, channel)
- Issue 08 (summarization prompt — `PromptBuilder`)

## Steps

1. **`CompressionWorker` struct**:
   ```rust
   pub struct CompressionWorker {
       rx: mpsc::Receiver<CompressionJob>,
       store: Arc<dyn TranscriptStore>,
       estimator: Arc<TokenEstimator>,
       prompt_builder: Arc<PromptBuilder>,
       backend_url: url::Url,
       client: reqwest::Client,
       config: ContextPolicy,
       metrics: Arc<Metrics>,
   }
   ```

2. **`CompressionWorker::run()`** — main loop:
   ```rust
   pub async fn run(mut self) {
       tracing::info!("context compression worker started");
       while let Some(job) = self.rx.recv().await {
           if let Err(e) = self.process_job(&job).await {
               tracing::warn!(flow_id = %job.flow_id, error = %e, "compression job failed");
           }
       }
       tracing::info!("context compression worker stopped");
   }
   ```
   - The worker processes jobs sequentially (one at a time). This is
     intentional — we don't want multiple sidecar requests competing for
     vLLM slots simultaneously.
   - If the channel is full when `trigger_compression` tries to send,
     the job is dropped (issue 06). The next request for that flow will
     re-evaluate and re-enqueue if still over threshold.

3. **`process_job()`** — per-job logic:
   ```rust
   async fn process_job(&self, job: &CompressionJob) -> anyhow::Result<()>
   ```

   a. **Acquire per-flow lock** (same `flow_locks` from `ContextState`):
      - Wait for the lock — this ensures no request is mid-reconcile
        while we mutate the transcript.
      - Hold the lock for the entire job (including the sidecar request).
        This is acceptable because compression is background and the lock
        only blocks other requests for the same flow, not other flows.

   b. **Load transcript** from store.

   c. **Extract messages to compress**:
      - Get the Live segment's `raw_messages`
      - Slice `turn_range_start..turn_range_end` using turn boundaries
      - These are the original messages that will be summarized

   d. **Build summarization prompt**:
      ```rust
      let prompt = self.prompt_builder.build(
          &messages_to_compress,
          job.turn_range_start,
          job.turn_range_end,
      );
      ```

   e. **Make sidecar request** to vLLM:
      ```rust
      let sidecar_body = serde_json::json!({
          "model": "compressor",
          "messages": prompt,
          "max_tokens": self.config.summary_max_tokens,
          "temperature": 0.3,
          "stream": false,
      });
      let resp = self.client
          .post(self.backend_url.join("v1/chat/completions")?)
          .header("X-LLM-Flow-ID", format!("compressor:{}", job.flow_id))
          .header("X-LLM-Internal", "compressor")
          .json(&sidecar_body)
          .timeout(self.config.sidecar_request_timeout.into())
          .send()
          .await?;
      ```

   f. **Extract summary text** from response:
      - Parse JSON response
      - Extract `choices[0].message.content`
      - If response is error or content empty: treat as failure

   g. **Create Compressed segment**:
      ```rust
      let summary_message = json!({"role": "system", "content": summary_text});
      let summary_tokens = self.estimator.estimate_text(&summary_text);
      let raw_tokens = self.estimator.estimate_messages(&messages_to_compress);
      let segment = Segment {
          flow_id: job.flow_id.clone(),
          segment_idx: next_compressed_idx,
          kind: SegmentKind::Compressed,
          raw_messages: messages_to_compress,
          summary_message: Some(summary_message),
          msg_start_idx: ...,
          msg_end_idx: ...,
          est_tokens: summary_tokens,
          raw_est_tokens: raw_tokens,
          created_at: Utc::now(),
      };
      ```

   h. **Update Live segment**:
      - Remove the compressed turns from Live's `raw_messages`
      - Update `msg_start_idx` to reflect the new boundary
      - Recalculate Live's `est_tokens` and `raw_est_tokens`

   i. **Save to store**:
      - `store.save_segment(&compressed_segment)`
      - `store.update_live_segment(&updated_live)`
      - `store.upsert_meta(&updated_meta)` (recount totals)

   j. **Release lock** (via `with_flow_lock` RAII)

4. **Retry logic**:
   ```rust
   // In process_job, wrap the sidecar request:
   for attempt in 0..self.config.compression_retries {
       match self.call_sidecar(&prompt).await {
           Ok(summary) => break Ok(summary),
           Err(e) if attempt < self.config.compression_retries - 1 => {
               let backoff = Duration::from_secs(1 << attempt); // 1s, 2s, 4s
               tokio::time::sleep(backoff).await;
               continue;
           }
           Err(e) => return Err(e),
       }
   }
   ```

5. **Sidecar request construction details**:
   - `model` field: set to `"compressor"` (vLLM ignores this for a single-model
     server, but it's required by the API spec)
   - `temperature`: 0.3 (low creativity, more factual summaries)
   - `stream`: false (we need the full summary synchronously)
   - **No `tools` field** — the summarization prompt is a plain chat
     completion, no tool calling
   - The `X-LLM-Internal: compressor` header ensures `rewrite_messages`
     (issue 07) skips this request
   - The `X-LLM-Flow-ID: compressor:{original_flow_id}` ensures the
     scheduler tracks it as a separate flow with `background` priority
     (set via `POST /flows` or the priority system)

6. **Priority tagging**: register the compressor flow as background priority:
   - At startup, call the admin API or directly register in `FlowRegistry`:
     ```rust
     flow_registry.register(FlowRegistration {
         id: "compressor:*".into(),  // wildcard? or register per-flow on demand
         weight: 1,
         priority: config.priorities.background,
     });
     ```
   - Alternatively: the sidecar request includes a
     `metadata.priority: "background"` field that the flow identify logic
     picks up (if implemented). For now, use the `X-LLM-Flow-ID` prefix
     `compressor:` and register it as background priority.

7. **Startup in `main.rs`**:
   ```rust
   if let Some(ref ctx) = context_state {
       let worker = CompressionWorker::new(
           rx, ctx.store.clone(), ctx.estimator.clone(),
           ctx.prompt_builder.clone(), state.backend_url.clone(),
           state.client.clone(), ctx.config.clone(), state.metrics.clone(),
       );
       tokio::spawn(async move { worker.run().await });
   }
   ```

8. **Startup scan** (from issue 06): after `ContextState` is created, call
   `find_flows_needing_compression()` which enqueues jobs for all flows
   over threshold. The worker (spawned next) will process them.

9. **Graceful shutdown**: when the mpsc sender is dropped (all senders go
   out of scope), `rx.recv()` returns `None` and the worker exits cleanly.
   This happens naturally on proxy shutdown.

## Tests

- `test_worker_processes_job` — enqueue a job, mock sidecar returns summary,
  verify Compressed segment stored + Live shrunk
- `test_worker_retries_on_failure` — mock sidecar fails twice then succeeds,
  verify 3 attempts made
- `test_worker_drops_job_after_max_retries` — all attempts fail, verify
  error logged and no corruption
- `test_sidecar_request_headers` — verify `X-LLM-Internal: compressor` and
  `X-LLM-Flow-ID: compressor:{flow_id}` headers sent
- `test_sidecar_skips_compression` — sidecar request body passes through
  `rewrite_messages` unchanged (verified by sending through the full proxy
  in an integration test)
- `test_concurrent_jobs_different_flows` — two jobs for different flows
  processed sequentially (worker is single-threaded by design)
- `test_lock_prevents_concurrent_reconcile` — while worker holds the lock,
  a request for the same flow blocks until worker finishes

## Verification

```bash
cargo test --lib compressor 2>&1 | tail -10
cargo test --lib test_worker 2>&1 | tail -10
```

## Notes

- The worker is single-threaded (sequential job processing). If compression
  throughput becomes a bottleneck, we can spawn multiple workers sharing
  the channel, but for now the single-worker model is simpler and avoids
  concurrent sidecar requests competing for vLLM slots.
- The sidecar request goes through the proxy itself (localhost:1234 →
  proxy → vLLM :8000). This means the scheduler's backpressure applies
  to compression requests — if vLLM is busy, compression waits. This is
  desirable (compression is background work).
- If the sidecar request times out, the job fails and will be re-enqueued
  on the next request for that flow (which re-checks the threshold).
