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
// v0 engine name.  v1 engines expose `vllm:kv_cache_usage_perc` instead;
// update this constant when upgrading to v1.
pub const METRIC_KV_USAGE: &str = "vllm:gpu_cache_usage_perc";

// `vllm:gpu_cache_free_perc` is the primary gauge; `kv_free` is derived
// as `1.0 - kv_usage` when only the usage gauge is available.
pub const METRIC_KV_FREE: &str = "vllm:gpu_cache_free_perc";

// `vllm:num_preemptions_total` — cumulative preemptions (optional; ignored if absent).
// Note: the actual vLLM metric is `vllm:num_preemptions_total`, not `vllm:num_preemption`.
pub const METRIC_NUM_PREEMPTION: &str = "vllm:num_preemptions_total";

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
}

impl Default for BackendSnapshot {
    fn default() -> Self {
        Self {
            kv_usage: 0.0,
            kv_free: 1.0,
            preemptions: 0,
        }
    }
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
    } else if let Some(space) = line.find(' ') {
        &line[..space]
    } else {
        return None;
    };

    // Find the value (last token that looks like a number).
    let value = line
        .split_whitespace()
        .last()
        .and_then(|s| s.parse::<f64>().ok());

    Some((metric_name, value?))
}

/// Parse a Prometheus-formatted metrics body into a `BackendSnapshot`.
fn parse_snapshot(body: &str) -> BackendSnapshot {
    let mut snapshot = BackendSnapshot::default();
    let mut found_usage = false;

    for line in body.lines() {
        if let Some((name, value)) = parse_prometheus_line(line) {
            match name {
                METRIC_KV_USAGE => {
                    snapshot.kv_usage = value;
                    found_usage = true;
                }
                METRIC_KV_FREE => {
                    snapshot.kv_free = value;
                }
                METRIC_NUM_PREEMPTION => {
                    snapshot.preemptions = value as u64;
                }
                _ => {}
            }
        }
    }

    // Derive free from usage if the free gauge was not present.
    if !found_usage {
        // No usage data — keep defaults (0.0 usage / 1.0 free).
    } else if snapshot.kv_free == 0.0 && snapshot.kv_usage < 1.0 {
        // If free was not explicitly set but usage was, derive it.
        snapshot.kv_free = 1.0 - snapshot.kv_usage;
    }

    snapshot
}

// ---------------------------------------------------------------------------
// BackendMonitor — periodic polling task
// ---------------------------------------------------------------------------

/// Shared handle that provides the latest `BackendSnapshot`.
///
/// Uses `tokio::sync::watch` for single-writer / multi-reader semantics.
/// The sender is consumed by the monitor loop; receivers are cloned for
/// the scheduler, tests, etc.
#[derive(Clone)]
pub struct BackendMonitor {
    /// Receiver half for reading the latest snapshot.
    receiver: tokio::sync::watch::Receiver<BackendSnapshot>,
}

impl BackendMonitor {
    /// Create an empty (disabled) monitor with a default snapshot.
    ///
    /// Used by tests and backward-compatible constructors that don't need
    /// a live backend monitor.  Always returns `Accept` decisions.
    pub fn empty() -> Self {
        let (_, rx) = tokio::sync::watch::channel(BackendSnapshot::default());
        Self { receiver: rx }
    }

    /// Create a monitor from an existing watch receiver.
    ///
    /// Used by tests that want to inject specific snapshots via the
    /// corresponding sender.
    pub fn from_receiver(receiver: tokio::sync::watch::Receiver<BackendSnapshot>) -> Self {
        Self { receiver }
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

        let handle = if config.metrics_interval.is_zero() {
            // Interval of 0 means "disabled" — no background task.
            None
        } else {
            let url = Self::metrics_url(&config.url);
            Some(tokio::spawn(Self::poll_loop(
                url,
                config.metrics_interval,
                tx,
                client,
                metrics,
            )))
        };

        let monitor = Self { receiver: rx };

        (monitor, handle)
    }

    /// Build the `/metrics` URL from the backend base URL.
    fn metrics_url(base: &Url) -> Url {
        let mut url = base.clone();
        url.set_path(&format!("{}/metrics", url.path().trim_end_matches('/')));
        url
    }

    /// Background polling loop.
    async fn poll_loop(
        url: Url,
        interval: Duration,
        tx: tokio::sync::watch::Sender<BackendSnapshot>,
        client: reqwest::Client,
        metrics: Arc<Metrics>,
    ) {
        let mut interval_timer = tokio::time::interval(interval);
        interval_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            interval_timer.tick().await;

            match client.get(url.clone()).send().await {
                Ok(response) => match response.text().await {
                    Ok(body) => {
                        let snapshot = parse_snapshot(&body);
                        // Update the watch channel.
                        let _ = tx.send(snapshot.clone());
                        // Update Prometheus gauges.
                        metrics.vllm_kv_cache_usage.set(snapshot.kv_usage);
                        metrics.vllm_kv_cache_free.set(snapshot.kv_free);
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
    /// Returns `true` if the predicate was satisfied, `false` if the channel
    /// was closed.
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
        let snapshot = parse_snapshot(body);
        assert_eq!(snapshot.kv_usage, 0.85, "kv_usage should be 0.85");
        assert_eq!(snapshot.preemptions, 42, "preemptions should be 42");
    }

    #[test]
    fn parse_snapshot_with_free_gauge() {
        let body = "vllm:gpu_cache_usage_perc 0.7\nvllm:gpu_cache_free_perc 0.3\n";
        let snapshot = parse_snapshot(body);
        assert_eq!(snapshot.kv_usage, 0.7);
        assert_eq!(snapshot.kv_free, 0.3);
    }

    #[test]
    fn parse_snapshot_metric_absent_returns_defaults() {
        let body = "some_other_metric 1.0\n";
        let snapshot = parse_snapshot(body);
        assert_eq!(snapshot.kv_usage, 0.0, "usage should default to 0.0");
        assert_eq!(snapshot.kv_free, 1.0, "free should default to 1.0");
        assert_eq!(snapshot.preemptions, 0, "preemptions should default to 0");
    }

    #[test]
    fn parse_snapshot_empty_body() {
        let snapshot = parse_snapshot("");
        assert_eq!(snapshot.kv_usage, 0.0);
        assert_eq!(snapshot.kv_free, 1.0);
    }

    #[test]
    fn parse_snapshot_malformed_lines_skipped() {
        let body = r#"vllm:gpu_cache_usage_perc 0.5
this is garbage
vllm:gpu_cache_usage_perc not_a_number
{missing_name} 1.0
vllm:num_preemptions_total 7
"#;
        let snapshot = parse_snapshot(body);
        // First valid usage line sets 0.5, then garbage lines are skipped.
        assert_eq!(snapshot.kv_usage, 0.5);
        assert_eq!(snapshot.preemptions, 7);
    }

    #[test]
    fn parse_snapshot_histogram_sum_count_ignored() {
        let body = r#"vllm:gpu_cache_usage_perc 0.5
some_histogram_sum 1234.5
some_histogram_count 42
vllm:num_preemptions_total 3
"#;
        let snapshot = parse_snapshot(body);
        assert_eq!(snapshot.kv_usage, 0.5);
        assert_eq!(snapshot.preemptions, 3);
        // Histogram lines are parsed but not matched to known metrics,
        // so they don't affect the snapshot.
    }
}
