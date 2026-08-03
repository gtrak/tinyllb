//! KV-cache-aware admission policy (PRD §6.3).
//!
//! Reads the latest `BackendSnapshot` from the `BackendMonitor` and decides
//! whether to accept, delay, or reject a request based on KV-cache pressure.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::backend::BackendMonitor;
use crate::config::{BackpressureMode, KvPolicyConfig};
use crate::metrics::Metrics;
use crate::scheduler::backpressure::{fail_fast_retry_after, BackpressureRejected};

// ---------------------------------------------------------------------------
// Decision
// ---------------------------------------------------------------------------

/// Outcome of a KV-cache admission check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KVMDecision {
    /// KV pressure is low enough — proceed to the flow scheduler.
    Accept,
    /// KV pressure is between delay and reject thresholds.  Park the request
    /// and wait for usage to drop below `delay_threshold`.
    Delay,
    /// KV pressure exceeds the reject threshold.  Return 429 with Retry-After.
    Reject(Duration),
}

// ---------------------------------------------------------------------------
// KvPolicy
// ---------------------------------------------------------------------------

/// KV-cache-aware admission gate.
///
/// Holds a shared `BackendMonitor` reference and threshold configuration.
/// Mirrors the pattern used by `CompletionBiasGate`: a pre-admit check that
/// can block (delay) or reject before the request reaches the flow scheduler.
///
/// ## Delay timeout design
///
/// For **blocking** mode, the delay wait is unbounded (the blocking contract:
/// the request waits indefinitely for KV pressure to drop).
///
/// For **hybrid** and **failfast** modes, the delay wait is bounded by
/// `max_wait`. If usage does not drop below the delay threshold within
/// `max_wait`, the request is rejected with `BackpressureRejected` (429).
/// This preserves the backpressure timeout contract from issue 06: hybrid
/// and failfast modes must always return within a bounded time.
///
/// ## Queue visibility
///
/// Delayed requests are counted in `queue_depth()` and `queue_snapshot()`
/// via `delayed_count`. This ensures the failfast `max_queue_depth` check
/// sees them and GET /queue reports them as waiting.
// @lat: [[admission#KV-Cache-Aware Admission Gate]]
pub struct KvPolicy {
    /// Whether KV policy is enabled.
    enabled: bool,
    /// Reject if `kv_usage > reject_threshold`.
    reject_threshold: f64,
    /// Delay if `kv_usage > delay_threshold` (but below reject).
    delay_threshold: f64,
    /// Shared monitor for reading latest snapshots.
    monitor: Arc<BackendMonitor>,
    /// Metrics handle for recording decisions.
    metrics: Arc<Metrics>,
    /// Backpressure mode, used to determine delay wait behavior.
    backpressure_mode: BackpressureMode,
    /// Max wait duration for hybrid/failfast delay timeout.
    /// For blocking mode the delay wait is unbounded.
    max_wait: Duration,
    /// Base duration for Retry-After computation on delay timeout.
    retry_after_base: Duration,
    /// Max queue depth for Retry-After scaling on delay timeout.
    max_queue_depth: u32,
    /// Number of requests currently waiting in the delay loop.
    /// Incremented when a request enters the delay wait, decremented when
    /// it proceeds or is rejected.  Counted in queue_depth / queue_snapshot.
    delayed_count: AtomicU32,
}

impl KvPolicy {
    /// Create a new KV policy from config.
    ///
    /// `backpressure_mode`, `max_wait`, `retry_after_base`, and
    /// `max_queue_depth` are threaded through from the scheduler's
    /// backpressure configuration so that the delay wait honors the
    /// issue-06 timeout contract.
    pub fn new(
        config: &KvPolicyConfig,
        monitor: Arc<BackendMonitor>,
        metrics: Arc<Metrics>,
        backpressure_mode: BackpressureMode,
        max_wait: Duration,
        retry_after_base: Duration,
        max_queue_depth: u32,
    ) -> Self {
        Self {
            enabled: config.enabled,
            reject_threshold: config.reject_threshold,
            delay_threshold: config.delay_threshold,
            monitor,
            metrics: metrics.clone(),
            backpressure_mode,
            max_wait,
            retry_after_base,
            max_queue_depth,
            delayed_count: AtomicU32::new(0),
        }
    }

    /// Decide whether to accept, delay, or reject based on the latest snapshot.
    ///
    /// Thresholds are strict: reject when `kv_usage > reject_threshold`,
    /// delay when `kv_usage > delay_threshold`.
    fn decide(&self, snapshot: &crate::backend::BackendSnapshot) -> KVMDecision {
        if snapshot.kv_usage > self.reject_threshold {
            // Reject with a Retry-After proportional to how far above threshold.
            let excess = snapshot.kv_usage - self.reject_threshold;
            // Base 5s, scale by excess fraction (up to ~5s at 1.0 usage).
            let retry_after = Duration::from_secs_f64(5.0 + excess * 10.0);
            KVMDecision::Reject(retry_after)
        } else if snapshot.kv_usage > self.delay_threshold {
            KVMDecision::Delay
        } else {
            KVMDecision::Accept
        }
    }

    /// Number of requests currently waiting in the KV-delay loop.
    ///
    /// Included in `Scheduler::queue_depth()` and `queue_snapshot()` so that
    /// delayed requests are visible to the failfast max_queue_depth check and
    /// GET /queue reports.
    pub fn delayed_count(&self) -> u32 {
        self.delayed_count.load(Ordering::Relaxed)
    }

    /// Check KV-cache pressure before admitting a request.
    ///
    /// - If KV policy is disabled, always proceeds.
    /// - If KV usage exceeds `reject_threshold`, returns `Err(BackpressureRejected)`.
    /// - If KV usage exceeds `delay_threshold`, waits for usage to drop below
    ///   `delay_threshold` before proceeding.
    ///   - **Blocking**: waits indefinitely (blocking contract).
    ///   - **Hybrid/FailFast**: waits up to `max_wait`; rejects with 429 on timeout.
    /// - Otherwise, proceeds immediately.
    pub async fn check(&self) -> Result<(), BackpressureRejected> {
        // KV policy disabled — always proceed.
        if !self.enabled {
            return Ok(());
        }

        let snapshot = match self.monitor.snapshot() {
            Some(s) => s,
            None => {
                // Monitor closed — default to accept (don't reject on monitor failure).
                tracing::warn!("backend monitor channel closed, defaulting to accept");
                return Ok(());
            }
        };

        match self.decide(&snapshot) {
            KVMDecision::Accept => {
                self.metrics
                    .kv_admission_decisions_total
                    .with_label_values(&["accept"])
                    .inc();
                Ok(())
            }
            KVMDecision::Delay => {
                self.metrics
                    .kv_admission_decisions_total
                    .with_label_values(&["delay"])
                    .inc();
                // Count this request in queue_depth while it's delayed.
                let _delay_guard = DelayGuard::new(&self.delayed_count);

                // Wait for KV pressure to drop below delay_threshold.
                // Timeout behavior depends on backpressure mode:
                // - Blocking: unbounded wait
                // - Hybrid/FailFast: bounded by max_wait
                let result = match self.backpressure_mode {
                    BackpressureMode::Blocking => {
                        self.monitor
                            .wait_for(|s| s.kv_usage <= self.delay_threshold)
                            .await;
                        Ok(())
                    }
                    BackpressureMode::Hybrid | BackpressureMode::FailFast => {
                        tokio::time::timeout(
                            self.max_wait,
                            self.monitor
                                .wait_for(|s| s.kv_usage <= self.delay_threshold),
                        )
                        .await
                        .map(|_| Ok(()))
                        .unwrap_or_else(|_| {
                            // Delay wait timed out — reject with backpressure 429.
                            // Use the same Retry-After formula as the flow scheduler.
                            let depth = self.delayed_count.load(Ordering::Relaxed);
                            let retry_after = fail_fast_retry_after(
                                depth,
                                self.max_queue_depth,
                                self.retry_after_base,
                            );
                            Err(BackpressureRejected { retry_after })
                        })
                    }
                };

                // Guard decrements delayed_count on drop.
                drop(_delay_guard);
                result
            }
            KVMDecision::Reject(retry_after) => {
                self.metrics
                    .kv_admission_decisions_total
                    .with_label_values(&["reject"])
                    .inc();
                Err(BackpressureRejected { retry_after })
            }
        }
    }
}

/// RAII guard that decrements the delayed count on drop.
///
/// Ensures the count is decremented regardless of whether the delay
/// wait succeeds, times out, or is cancelled.
struct DelayGuard<'a> {
    count: &'a AtomicU32,
}

impl<'a> DelayGuard<'a> {
    fn new(count: &'a AtomicU32) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        Self { count }
    }
}

impl Drop for DelayGuard<'_> {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::Relaxed);
    }
}
