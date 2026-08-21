//! Backend monitoring module.
//!
//! Periodically polls the vLLM `/metrics` endpoint and provides a typed
//! `BackendSnapshot` for KV-cache-aware admission decisions.
//!
//! The vLLM metric names are stored as constants so that a version bump
//! only requires editing this file.  See comments for each constant.

use std::sync::Arc;
use std::time::Duration;

use url::Url;

use crate::config::Backend as BackendConfig;
use crate::metrics::Metrics;

// ---------------------------------------------------------------------------
// vLLM metric name constants
// ---------------------------------------------------------------------------
// These are the well-known vLLM metric names.  If your vLLM version uses
// different names, update these constants and recompile.
//
// Confirm against your vLLM's `/metrics` output:
//   curl http://localhost:8000/metrics | grep vllm
//
// `vllm:gpu_cache_usage_perc` — fraction of KV cache blocks in use [0..1].
// v0 engine name.
pub const METRIC_KV_USAGE: &str = "vllm:gpu_cache_usage_perc";

// `vllm:kv_cache_usage_perc` — v1 engine name for KV cache usage [0..1].
//
// v1 engines use this metric name instead of `vllm:gpu_cache_usage_perc`.
// The parser matches both names so that v0 and v1 backends work without
// configuration changes.
pub const METRIC_KV_USAGE_V1: &str = "vllm:kv_cache_usage_perc";

// `vllm:gpu_cache_free_perc` is the primary gauge; `kv_free` is derived
// as `1.0 - kv_usage` when only the usage gauge is available.
pub const METRIC_KV_FREE: &str = "vllm:gpu_cache_free_perc";

// `vllm:num_preemptions_total` — cumulative preemptions (optional; ignored if absent).
// Note: the actual vLLM metric is `vllm:num_preemptions_total`, not `vllm:num_preemption`.
pub const METRIC_NUM_PREEMPTION: &str = "vllm:num_preemptions_total";

// `vllm:generation_tokens_total` — cumulative decode tokens. Frozen value
// while requests are queued/running is the inference-deadlock signal.
pub const METRIC_GENERATION_TOKENS: &str = "vllm:generation_tokens_total";

// `vllm:prompt_tokens_total` — cumulative prefill tokens. Also tracked so
// all-prefill workloads are not misclassified as stalled.
pub const METRIC_PROMPT_TOKENS: &str = "vllm:prompt_tokens_total";

// `vllm:num_requests_running` — requests currently scheduled on the engine.
pub const METRIC_REQUESTS_RUNNING: &str = "vllm:num_requests_running";

// `vllm:num_requests_waiting` — requests queued for the engine.
pub const METRIC_REQUESTS_WAITING: &str = "vllm:num_requests_waiting";

// ---------------------------------------------------------------------------
// llama.cpp metric name constants
// ---------------------------------------------------------------------------
// These are the well-known llama.cpp (`llama-server --metrics`) metric names.
// If your llama.cpp version uses different names, update these constants and
// recompile.
//
// Confirm against your llama-server's `/metrics` output:
//   curl http://localhost:8000/metrics | grep llamacpp
//
// Note: llama.cpp does not expose a KV-usage metric (removed upstream in
// llama.cpp#13660; re-add PR #24010 unmerged), so `found_usage` is always
// false for llama.cpp backends and `kv_usage` stays at its 0.0 default.

// `llamacpp:requests_processing` — requests currently being processed.
pub const METRIC_LLAMACPP_REQUESTS_PROCESSING: &str = "llamacpp:requests_processing";

// `llamacpp:requests_deferred` — requests deferred (queued) for processing.
pub const METRIC_LLAMACPP_REQUESTS_DEFERRED: &str = "llamacpp:requests_deferred";

// `llamacpp:prompt_tokens_total` — cumulative prefill tokens, excluding
// cached tokens (see `METRIC_LLAMACPP_CACHED_TOKENS`).
pub const METRIC_LLAMACPP_PROMPT_TOKENS: &str = "llamacpp:prompt_tokens_total";

// `llamacpp:tokens_predicted_total` — cumulative generated (decode) tokens.
pub const METRIC_LLAMACPP_PREDICTED_TOKENS: &str = "llamacpp:tokens_predicted_total";

// `llamacpp:prompt_tokens_cached_total` — cumulative tokens served from the
// prefix cache. Progress-only signal for the stall watchdog; no vLLM analog.
pub const METRIC_LLAMACPP_CACHED_TOKENS: &str = "llamacpp:prompt_tokens_cached_total";

// `llamacpp:n_decode_total` — every `llama_decode()` call, including prefill
// batches. Progress-only signal for the stall watchdog; no vLLM analog.
pub const METRIC_LLAMACPP_DECODE_CALLS: &str = "llamacpp:n_decode_total";

// ---------------------------------------------------------------------------
// Typed snapshot
// ---------------------------------------------------------------------------

/// Latest snapshot of vLLM backend state from `/metrics`.
#[derive(Debug, Clone)]
pub struct BackendSnapshot {
    /// KV cache usage fraction [0..1].
    pub kv_usage: f64,
    /// KV cache free fraction [0..1].
    pub kv_free: f64,
    /// Cumulative preemptions (best-effort; 0 if unavailable).
    pub preemptions: u64,
    /// Cumulative decode tokens (0 if unavailable).
    pub generation_tokens: f64,
    /// Cumulative prefill tokens (0 if unavailable).
    pub prompt_tokens: f64,
    /// Requests currently running on the engine (0 if unavailable).
    pub requests_running: f64,
    /// Requests waiting for the engine (0 if unavailable).
    pub requests_waiting: f64,
    /// Cumulative tokens served from the llama.cpp prefix cache (0 if
    /// unavailable). Progress-only signal for the stall watchdog; no vLLM
    /// analog.
    pub cached_prompt_tokens: f64,
    /// Cumulative `llama_decode()` calls, including prefill batches (0 if
    /// unavailable). Progress-only signal for the stall watchdog; no vLLM
    /// analog.
    pub decode_calls: f64,
}

impl Default for BackendSnapshot {
    fn default() -> Self {
        Self {
            kv_usage: 0.0,
            kv_free: 1.0,
            preemptions: 0,
            generation_tokens: 0.0,
            prompt_tokens: 0.0,
            requests_running: 0.0,
            requests_waiting: 0.0,
            cached_prompt_tokens: 0.0,
            decode_calls: 0.0,
        }
    }
}

impl BackendSnapshot {
    /// Whether the engine has queued or running work.
    pub fn is_busy(&self) -> bool {
        self.requests_running > 0.0 || self.requests_waiting > 0.0
    }
}

/// Result of parsing a Prometheus /metrics body.
///
/// Carries the snapshot plus per-metric found flags so callers can
/// distinguish "metric present with value 0" from "metric absent".
#[derive(Debug, Clone, Default)]
pub struct ParseSnapshotResult {
    /// Parsed snapshot values.
    pub snapshot: BackendSnapshot,
    /// Whether the KV usage metric was found in the body.
    pub found_usage: bool,
    /// Whether the KV free metric was found in the body.
    pub found_free: bool,
    /// Whether any llama.cpp (`llamacpp:*`) metric was found in the body.
    pub found_llamacpp: bool,
}

// ---------------------------------------------------------------------------
// Prometheus text-format line scanner
// ---------------------------------------------------------------------------

/// Parse a single line from Prometheus text exposition format.
///
/// Handles lines of the form:
///   `metric_name{label="value"} value`
///   `metric_name value`
///
/// Returns the metric name and optional value.
fn parse_prometheus_line(line: &str) -> Option<(&str, f64)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }

    // Find the metric name (everything before `{` or space).
    let metric_name = if let Some(brace) = line.find('{') {
        &line[..brace]
    } else {
        let space = line.find(' ')?;
        &line[..space]
    };

    // Find the value (last token that looks like a number).
    let value = line
        .split_whitespace()
        .last()
        .and_then(|s| s.parse::<f64>().ok());

    Some((metric_name, value?))
}

/// Parse a Prometheus-formatted metrics body into a `ParseSnapshotResult`.
///
/// Used by the BackendMonitor poll loop and by integration tests
/// that want to verify the live backend's /metrics output.
// @lat: [[backend#Backend Metrics Parsing]]
pub fn parse_snapshot(body: &str) -> ParseSnapshotResult {
    let mut snapshot = BackendSnapshot::default();
    let mut found_usage = false;
    let mut found_free = false;
    let mut found_llamacpp = false;

    for line in body.lines() {
        if let Some((name, value)) = parse_prometheus_line(line) {
            match name {
                METRIC_KV_USAGE | METRIC_KV_USAGE_V1 => {
                    snapshot.kv_usage = value;
                    found_usage = true;
                }
                METRIC_KV_FREE => {
                    snapshot.kv_free = value;
                    found_free = true;
                }
                METRIC_NUM_PREEMPTION => {
                    snapshot.preemptions = value as u64;
                }
                METRIC_GENERATION_TOKENS => {
                    snapshot.generation_tokens = value;
                }
                METRIC_PROMPT_TOKENS => {
                    snapshot.prompt_tokens = value;
                }
                METRIC_REQUESTS_RUNNING => {
                    snapshot.requests_running = value;
                }
                METRIC_REQUESTS_WAITING => {
                    snapshot.requests_waiting = value;
                }
                METRIC_LLAMACPP_REQUESTS_PROCESSING => {
                    snapshot.requests_running = value;
                    found_llamacpp = true;
                }
                METRIC_LLAMACPP_REQUESTS_DEFERRED => {
                    snapshot.requests_waiting = value;
                    found_llamacpp = true;
                }
                METRIC_LLAMACPP_PROMPT_TOKENS => {
                    snapshot.prompt_tokens = value;
                    found_llamacpp = true;
                }
                METRIC_LLAMACPP_PREDICTED_TOKENS => {
                    snapshot.generation_tokens = value;
                    found_llamacpp = true;
                }
                METRIC_LLAMACPP_CACHED_TOKENS => {
                    snapshot.cached_prompt_tokens = value;
                    found_llamacpp = true;
                }
                METRIC_LLAMACPP_DECODE_CALLS => {
                    snapshot.decode_calls = value;
                    found_llamacpp = true;
                }
                _ => {}
            }
        }
    }

    // Derive free from usage if the free gauge was not present but usage was.
    if found_usage && !found_free && snapshot.kv_usage < 1.0 {
        snapshot.kv_free = 1.0 - snapshot.kv_usage;
    }

    ParseSnapshotResult {
        snapshot,
        found_usage,
        found_free,
        found_llamacpp,
    }
}

// ---------------------------------------------------------------------------
// BackendMonitor — periodic polling task
// ---------------------------------------------------------------------------

/// Shared handle that provides the latest `BackendSnapshot`.
///
/// Uses `tokio::sync::watch` for single-writer / multi-reader semantics.
/// The sender is consumed by the monitor loop; receivers are cloned for
/// the scheduler, tests, etc.
///
/// Also exposes a stall channel: `stall_receiver()` yields `true` while
/// the inference watchdog considers the engine deadlocked (busy but no
/// token progress for `stall_timeout`). Stream handlers select on this
/// signal to abort in-flight backend streams.
// @lat: [[backend#Backend KV-Cache Monitor]]
#[derive(Clone)]
pub struct BackendMonitor {
    /// Receiver half for reading the latest snapshot.
    receiver: tokio::sync::watch::Receiver<BackendSnapshot>,
    /// Receiver half for the stall signal (`true` = deadlocked).
    stall_receiver: tokio::sync::watch::Receiver<bool>,
}

impl BackendMonitor {
    /// Create an empty (disabled) monitor with a default snapshot.
    ///
    /// Used by tests and backward-compatible constructors that don't need
    /// a live backend monitor.  Always returns `Accept` decisions.
    pub fn empty() -> Self {
        let (_, rx) = tokio::sync::watch::channel(BackendSnapshot::default());
        let (_, stall_rx) = tokio::sync::watch::channel(false);
        Self {
            receiver: rx,
            stall_receiver: stall_rx,
        }
    }

    /// Create a monitor from an existing watch receiver.
    ///
    /// Used by tests that want to inject specific snapshots via the
    /// corresponding sender.
    pub fn from_receiver(receiver: tokio::sync::watch::Receiver<BackendSnapshot>) -> Self {
        let (_, stall_rx) = tokio::sync::watch::channel(false);
        Self {
            receiver,
            stall_receiver: stall_rx,
        }
    }

    /// Clone the stall signal receiver.
    ///
    /// `true` means the engine is considered deadlocked (busy with no
    /// token progress). Streams select on `changed()` of this receiver
    /// and check `borrow()` to abort and retry.
    pub fn stall_receiver(&self) -> tokio::sync::watch::Receiver<bool> {
        self.stall_receiver.clone()
    }

    /// Create a new monitor with an initial default snapshot.
    ///
    /// Returns the monitor handle and a `tokio::Task` that you should spawn.
    pub fn new(
        config: &BackendConfig,
        metrics: Arc<Metrics>,
        client: reqwest::Client,
    ) -> (Self, Option<tokio::task::JoinHandle<()>>) {
        let (tx, rx) = tokio::sync::watch::channel(BackendSnapshot::default());
        let (stall_tx, stall_rx) = tokio::sync::watch::channel(false);

        let handle = if config.metrics_interval.is_zero() {
            // Interval of 0 means "disabled" — no background task.
            None
        } else {
            let url = Self::metrics_url(&config.url);
            Some(tokio::spawn(Self::poll_loop(
                url,
                config.metrics_interval,
                config.stall_timeout,
                tx,
                stall_tx,
                client,
                metrics,
            )))
        };

        let monitor = Self {
            receiver: rx,
            stall_receiver: stall_rx,
        };

        (monitor, handle)
    }

    /// Build the `/metrics` URL from the backend base URL.
    fn metrics_url(base: &Url) -> Url {
        let mut url = base.clone();
        url.set_path(&format!("{}/metrics", url.path().trim_end_matches('/')));
        url
    }

    /// Background polling loop.
    // @lat: [[backend#Inference Stall Watchdog]]
    async fn poll_loop(
        url: Url,
        interval: Duration,
        stall_timeout: Duration,
        tx: tokio::sync::watch::Sender<BackendSnapshot>,
        stall_tx: tokio::sync::watch::Sender<bool>,
        client: reqwest::Client,
        metrics: Arc<Metrics>,
    ) {
        let mut interval_timer = tokio::time::interval(interval);
        interval_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Inference-watchdog state (see vllm-watchdog.sh lineage): the
        // engine is deadlocked when requests are queued/running but
        // neither the prefill nor decode token counters advance.
        let mut last_prompt_tokens: f64 = 0.0;
        let mut last_generation_tokens: f64 = 0.0;
        let mut last_cached_tokens: f64 = 0.0;
        let mut last_decode_calls: f64 = 0.0;
        let mut last_progress = std::time::Instant::now();
        let mut stalled = false;
        let mut last_flavor: Option<&'static str> = None;

        loop {
            interval_timer.tick().await;

            match client.get(url.clone()).send().await {
                Ok(response) => match response.text().await {
                    Ok(body) => {
                        let result = parse_snapshot(&body);
                        // The flavor of the backend is identified per-scrape
                        // by metric-name prefix (vllm: vs llamacpp:).
                        let flavor = if result.found_usage {
                            "vllm"
                        } else if result.found_llamacpp {
                            "llama-cpp"
                        } else {
                            "unknown"
                        };
                        // The KV gauges are vLLM-only: llama.cpp has no
                        // KV-usage metric, so they must never be written for
                        // a llama.cpp scrape.
                        if result.found_usage {
                            metrics.vllm_kv_cache_usage.set(result.snapshot.kv_usage);
                            metrics.vllm_kv_cache_free.set(result.snapshot.kv_free);
                        }
                        // Only update if the KV usage metric or any llama.cpp
                        // metric was actually present. An empty/partial scrape
                        // (e.g. only python_gc_* lines) returns defaults —
                        // writing those would overwrite the last good reading
                        // with zeros.
                        if result.found_usage || result.found_llamacpp {
                            let snapshot = result.snapshot.clone();
                            let _ = tx.send(snapshot.clone());

                            // Log when the detected backend flavor changes
                            // between scrapes (e.g. backend swap).
                            if last_flavor != Some(flavor) {
                                tracing::info!(flavor, "detected backend metrics flavor");
                                last_flavor = Some(flavor);
                            }

                            // --- Inference stall watchdog ---
                            if !stall_timeout.is_zero() {
                                // Reset last_progress when the backend is idle — no stall is
                                // possible without running requests, and a new request starting
                                // after an idle gap should not inherit the gap as a "stall".
                                if !snapshot.is_busy() {
                                    last_progress = std::time::Instant::now();
                                }

                                // `llamacpp:prompt_tokens_total` excludes cached
                                // tokens, so on cache-heavy llama.cpp workloads
                                // the cached and decode-call counters are the
                                // missing progress signals.
                                let progressed = snapshot.prompt_tokens != last_prompt_tokens
                                    || snapshot.generation_tokens != last_generation_tokens
                                    || snapshot.cached_prompt_tokens != last_cached_tokens
                                    || snapshot.decode_calls != last_decode_calls;
                                if progressed {
                                    last_progress = std::time::Instant::now();
                                }
                                last_prompt_tokens = snapshot.prompt_tokens;
                                last_generation_tokens = snapshot.generation_tokens;
                                last_cached_tokens = snapshot.cached_prompt_tokens;
                                last_decode_calls = snapshot.decode_calls;

                                let now_stalled = snapshot.is_busy()
                                    && last_progress.elapsed() >= stall_timeout;
                                if now_stalled && !stalled {
                                    tracing::warn!(
                                        stall_secs = last_progress.elapsed().as_secs(),
                                        running = snapshot.requests_running,
                                        waiting = snapshot.requests_waiting,
                                        "backend inference stall detected — aborting in-flight streams"
                                    );
                                    metrics.backend_stall_events_total.inc();
                                    stalled = true;
                                } else if !now_stalled && stalled {
                                    tracing::info!("backend inference stall cleared");
                                    stalled = false;
                                }
                                metrics.llm_backend_stalled.set(if stalled { 1.0 } else { 0.0 });
                                let _ = stall_tx.send(stalled);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to read /metrics body");
                    }
                },
                Err(e) => {
                    // Backend unreachable — keep last snapshot (don't reject on monitor failure).
                    tracing::warn!(error = %e, backend = %url, "backend /metrics unreachable");
                }
            }
        }
    }

    /// Read the latest snapshot.  Returns `None` if the channel is closed.
    pub fn snapshot(&self) -> Option<BackendSnapshot> {
        self.receiver.borrow().clone().into()
    }

    /// Block until the snapshot meets the predicate.
    ///
    /// Used by `KvPolicy` to wait for KV pressure to drop below the delay
    /// threshold before admitting a request.
    ///
    /// Returns once the predicate is satisfied.  If the snapshot channel is
    /// closed, returns after evaluating the predicate against the last known
    /// snapshot.
    pub async fn wait_for(&self, predicate: impl Fn(&BackendSnapshot) -> bool + Send + Sync) {
        let mut rx = self.receiver.clone();
        loop {
            if predicate(&rx.borrow()) {
                return;
            }
            // Wait for the next notification.
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests for parser
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_prometheus_line tests ----

    #[test]
    fn parse_line_simple_metric() {
        let result = parse_prometheus_line("some_metric 42.5");
        assert_eq!(result, Some(("some_metric", 42.5)));
    }

    #[test]
    fn parse_line_with_labels() {
        let result =
            parse_prometheus_line(r#"vllm:gpu_cache_usage_perc{model_name="llama-3-8b"} 0.85"#);
        assert_eq!(
            result,
            Some(("vllm:gpu_cache_usage_perc", 0.85)),
            "should parse metric with model_name label"
        );
    }

    #[test]
    fn parse_line_skips_comment() {
        let result = parse_prometheus_line("# TYPE vllm:gpu_cache_usage_perc gauge");
        assert!(result.is_none(), "should skip TYPE/HELP lines");
    }

    #[test]
    fn parse_line_skips_empty() {
        let result = parse_prometheus_line("");
        assert!(result.is_none(), "should skip empty lines");
    }

    #[test]
    fn parse_line_skips_whitespace_only() {
        let result = parse_prometheus_line("   ");
        assert!(result.is_none(), "should skip whitespace-only lines");
    }

    #[test]
    fn parse_line_histogram_bucket_skipped() {
        // Histogram _bucket lines have non-numeric suffixes like "+Inf"
        let result = parse_prometheus_line(r#"some_histogram_bucket{le="+Inf"} 100"#);
        // The value "100" is parseable as f64, so this actually parses.
        // The important thing is that we don't match it as a KV metric.
        assert!(
            result.is_some(),
            "histogram bucket lines parse as generic metrics"
        );
    }

    #[test]
    fn parse_line_garbage_no_number() {
        // A line with a name but no numeric value at the end.
        let result = parse_prometheus_line("garbage_metric NaN_value");
        assert!(
            result.is_none(),
            "should skip lines where last token is not a number"
        );
    }

    #[test]
    fn parse_line_multiple_labels() {
        let result = parse_prometheus_line(
            r#"vllm:gpu_cache_usage_perc{model_name="foo",gpu_device_ORDINAL="0"} 0.72"#,
        );
        assert_eq!(
            result,
            Some(("vllm:gpu_cache_usage_perc", 0.72)),
            "should parse metric with multiple labels"
        );
    }

    #[test]
    fn parse_line_negative_value() {
        let result = parse_prometheus_line("some_metric -0.5");
        assert_eq!(result, Some(("some_metric", -0.5)));
    }

    #[test]
    fn parse_line_scientific_notation() {
        let result = parse_prometheus_line("some_metric 1.5e-2");
        assert_eq!(result, Some(("some_metric", 0.015)));
    }

    // ---- parse_snapshot tests ----

    #[test]
    fn parse_snapshot_realistic_vllm_metrics() {
        let body = r#"# HELP vllm:gpu_cache_usage_perc KV cache usage.
# TYPE vllm:gpu_cache_usage_perc gauge
vllm:gpu_cache_usage_perc{model_name="llama-3-8b"} 0.85
# HELP vllm:num_preemptions_total Number of preemptions.
# TYPE vllm:num_preemptions_total counter
vllm:num_preemptions_total 42
# HELP some_other_metric Something else.
# TYPE some_other_metric gauge
some_other_metric 99.9
"#;
        let result = parse_snapshot(body);
        assert_eq!(result.snapshot.kv_usage, 0.85, "kv_usage should be 0.85");
        assert_eq!(result.snapshot.preemptions, 42, "preemptions should be 42");
    }

    #[test]
    fn parse_snapshot_with_free_gauge() {
        let body = "vllm:gpu_cache_usage_perc 0.7\nvllm:gpu_cache_free_perc 0.3\n";
        let result = parse_snapshot(body);
        assert_eq!(result.snapshot.kv_usage, 0.7);
        assert_eq!(result.snapshot.kv_free, 0.3);
    }

    #[test]
    fn parse_snapshot_metric_absent_returns_defaults() {
        let body = "some_other_metric 1.0\n";
        let result = parse_snapshot(body);
        assert_eq!(result.snapshot.kv_usage, 0.0, "usage should default to 0.0");
        assert_eq!(result.snapshot.kv_free, 1.0, "free should default to 1.0");
        assert_eq!(
            result.snapshot.preemptions, 0,
            "preemptions should default to 0"
        );
    }

    #[test]
    fn parse_snapshot_empty_body() {
        let result = parse_snapshot("");
        assert_eq!(result.snapshot.kv_usage, 0.0);
        assert_eq!(result.snapshot.kv_free, 1.0);
    }

    #[test]
    fn parse_snapshot_malformed_lines_skipped() {
        let body = r#"vllm:gpu_cache_usage_perc 0.5
this is garbage
vllm:gpu_cache_usage_perc not_a_number
{missing_name} 1.0
vllm:num_preemptions_total 7
"#;
        let result = parse_snapshot(body);
        // First valid usage line sets 0.5, then garbage lines are skipped.
        assert_eq!(result.snapshot.kv_usage, 0.5);
        assert_eq!(result.snapshot.preemptions, 7);
    }

    #[test]
    fn parse_snapshot_histogram_sum_count_ignored() {
        let body = r#"vllm:gpu_cache_usage_perc 0.5
some_histogram_sum 1234.5
some_histogram_count 42
vllm:num_preemptions_total 3
"#;
        let result = parse_snapshot(body);
        assert_eq!(result.snapshot.kv_usage, 0.5);
        assert_eq!(result.snapshot.preemptions, 3);
        // Histogram lines are parsed but not matched to known metrics,
        // so they don't affect the snapshot.
    }

    // ---- v1 metric name tests ----

    #[test]
    fn parse_snapshot_v1_kv_cache_usage() {
        // v1 engine exposes `vllm:kv_cache_usage_perc` with engine label.
        let body = r#"# HELP vllm:kv_cache_usage_perc KV cache usage.
# TYPE vllm:kv_cache_usage_perc gauge
vllm:kv_cache_usage_perc{engine="0",model_name="local"} 0.42
vllm:num_preemptions_total{engine="0",model_name="local"} 1
"#;
        let result = parse_snapshot(body);
        assert_eq!(
            result.snapshot.kv_usage, 0.42,
            "v1 kv_cache_usage_perc should be parsed"
        );
        // kv_free derived as 1.0 - 0.42 = 0.58 (float tolerance)
        let kv_free = result.snapshot.kv_free;
        assert!(
            (kv_free - 0.58).abs() < 1e-9,
            "kv_free should be derived from kv_usage (expected ~0.58, got {})",
            kv_free
        );
        assert_eq!(result.snapshot.preemptions, 1);
    }

    #[test]
    fn parse_snapshot_v1_with_preemptions() {
        // Real vLLM v1 metrics output with engine/model labels.
        let body = r#"vllm:kv_cache_usage_perc{engine="0",model_name="local"} 0.0
vllm:num_preemptions_total{engine="0",model_name="local"} 1.0
"#;
        let result = parse_snapshot(body);
        assert_eq!(result.snapshot.kv_usage, 0.0);
        assert_eq!(result.snapshot.kv_free, 1.0);
        assert_eq!(result.snapshot.preemptions, 1);
    }

    #[test]
    fn parse_snapshot_v0_and_v1_both_present_v1_wins() {
        // If both v0 and v1 names appear (unlikely but possible in mixed
        // output), the last one wins (v1 overwrites v0).
        let body = "vllm:gpu_cache_usage_perc 0.75\nvllm:kv_cache_usage_perc 0.60\n";
        let result = parse_snapshot(body);
        assert_eq!(
            result.snapshot.kv_usage, 0.60,
            "v1 metric should overwrite v0 when both present"
        );
    }

    #[test]
    fn parse_snapshot_v0_still_works() {
        // Confirm existing v0 metric name still parses correctly.
        let body = "vllm:gpu_cache_usage_perc{model_name=\"llama-3-8b\"} 0.85\n";
        let result = parse_snapshot(body);
        assert_eq!(result.snapshot.kv_usage, 0.85);
        // kv_free derived as 1.0 - 0.85 = 0.15 (float tolerance)
        let kv_free = result.snapshot.kv_free;
        assert!(
            (kv_free - 0.15).abs() < 1e-9,
            "kv_free should be ~0.15, got {}",
            kv_free
        );
    }

    // ---- llama.cpp metric name tests ----

    #[test]
    fn parse_snapshot_realistic_llamacpp_metrics() {
        // Body copied from a live `llama-server --metrics` /metrics scrape.
        // The seven relevant lines: the six mapped gauges/counters plus
        // n_tokens_max (not mapped to any snapshot field; must be ignored).
        let body = r#"# HELP llamacpp:prompt_tokens_total Number of prompt tokens processed, excluding cached tokens
# TYPE llamacpp:prompt_tokens_total counter
llamacpp:prompt_tokens_total 77454
# HELP llamacpp:prompt_tokens_cached_total Number of prompt tokens reused from the cache
# TYPE llamacpp:prompt_tokens_cached_total counter
llamacpp:prompt_tokens_cached_total 129725
# HELP llamacpp:tokens_predicted_total Number of generation tokens processed
# TYPE llamacpp:tokens_predicted_total counter
llamacpp:tokens_predicted_total 1426
# HELP llamacpp:n_decode_total Total number of llama_decode() calls, excluding speculative decoding and multimodal decoding
# TYPE llamacpp:n_decode_total counter
llamacpp:n_decode_total 424
# HELP llamacpp:n_tokens_max Largest observed sequence length (prompt + generation)
# TYPE llamacpp:n_tokens_max counter
llamacpp:n_tokens_max 43372
# HELP llamacpp:requests_processing Number of requests processing
# TYPE llamacpp:requests_processing gauge
llamacpp:requests_processing 3
# HELP llamacpp:requests_deferred Number of requests deferred
# TYPE llamacpp:requests_deferred gauge
llamacpp:requests_deferred 0
"#;
        let result = parse_snapshot(body);
        assert!(result.found_llamacpp, "found_llamacpp should be true");
        assert!(!result.found_usage, "found_usage should be false");
        assert_eq!(result.snapshot.requests_running, 3.0);
        assert_eq!(result.snapshot.requests_waiting, 0.0);
        assert_eq!(result.snapshot.prompt_tokens, 77454.0);
        assert_eq!(result.snapshot.generation_tokens, 1426.0);
        assert_eq!(result.snapshot.cached_prompt_tokens, 129725.0);
        assert_eq!(result.snapshot.decode_calls, 424.0);
        assert_eq!(result.snapshot.kv_usage, 0.0, "kv_usage should default to 0.0");
        assert_eq!(result.snapshot.kv_free, 1.0, "kv_free should default to 1.0");
    }

    #[test]
    fn parse_snapshot_mixed_families_last_wins() {
        // A body containing both metric families. Both families write into
        // the same snapshot fields (last-parsed-wins per field, matching the
        // existing v0/v1 precedent): here the vllm request line overwrites
        // the earlier llamacpp one, while the later llamacpp prompt line
        // overwrites the earlier vllm one.
        let body = "llamacpp:requests_processing 3\nvllm:num_requests_running 1\nvllm:prompt_tokens_total 100\nllamacpp:prompt_tokens_total 999\nvllm:gpu_cache_usage_perc 0.5\n";
        let result = parse_snapshot(body);
        assert!(result.found_usage, "found_usage should be true");
        assert!(result.found_llamacpp, "found_llamacpp should be true");
        assert_eq!(
            result.snapshot.requests_running, 1.0,
            "later vllm line should win for requests_running"
        );
        assert_eq!(
            result.snapshot.prompt_tokens, 999.0,
            "later llamacpp line should win for prompt_tokens"
        );
        assert_eq!(result.snapshot.kv_usage, 0.5);
    }

    #[test]
    fn parse_snapshot_llamacpp_no_kv_metric() {
        // llama.cpp exposes no KV-usage metric, so a pure llamacpp body must
        // leave kv_usage at its 0.0 default and kv_free at 1.0 — no spurious
        // KV pressure on llama.cpp backends.
        let body = "llamacpp:requests_processing 2\nllamacpp:requests_deferred 1\nllamacpp:tokens_predicted_total 50\n";
        let result = parse_snapshot(body);
        assert!(!result.found_usage, "found_usage should stay false");
        assert_eq!(result.snapshot.kv_usage, 0.0);
        assert_eq!(result.snapshot.kv_free, 1.0);
    }

    #[test]
    fn parse_snapshot_llamacpp_flavor_flags() {
        // Each llamacpp metric line individually must set found_llamacpp
        // (and never found_usage).
        let bodies = [
            "llamacpp:requests_processing 1\n",
            "llamacpp:requests_deferred 2\n",
            "llamacpp:prompt_tokens_total 10\n",
            "llamacpp:tokens_predicted_total 20\n",
            "llamacpp:prompt_tokens_cached_total 30\n",
            "llamacpp:n_decode_total 40\n",
        ];
        for body in bodies {
            let result = parse_snapshot(body);
            assert!(
                result.found_llamacpp,
                "found_llamacpp should be set by {body:?}"
            );
            assert!(
                !result.found_usage,
                "found_usage must stay false for {body:?}"
            );
        }
    }
}
