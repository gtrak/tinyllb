//! KV-cache-aware selection bias.
//!
//! Under KV-cache pressure, prefers to grant the next permit to the eligible
//! flow holding the largest resident KV footprint (approximated by delivered
//! tokens), so that flow finishes and frees blocks instead of being preempted
//! and paged into/out of CPU-offloaded KV cache. This is a scheduling bias —
//! it only reorders which eligible flow wins a permit, never rejecting or
//! delaying a request.
//!
//! Pressure is derived from the backend's global KV usage gauge. The bias
//! strength ramps linearly from 0 (pure fairness) below `pressure_below` to
//! full dominance at/above `bias_full_at`. Footprint is normalized within a
//! selection round, so "all flows equal" collapses to the existing fairness
//! ordering (priority, then base score, then enqueue time).

use std::sync::Arc;

use crate::backend::BackendMonitor;
use crate::config::KvBias;
use crate::scheduler::flow_progress::FlowProgressTracker;
use crate::scheduler::priority::{self, FlowCandidate};

/// KV-cache-aware selection bias handle, shared with the schedulers.
///
/// Holds the configuration, the backend monitor (pressure source), and the
/// flow progress tracker (per-flow delivered-token footprint).
// @lat: [[scheduler_policies#KV-Cache-Aware Selection Bias]]
#[derive(Clone)]
pub struct KvBiasHandle {
    config: KvBias,
    monitor: Arc<BackendMonitor>,
    flow_progress: Arc<FlowProgressTracker>,
}

impl KvBiasHandle {
    /// Create a handle. `monitor` is used only as a pressure source; the
    /// admission gate is deliberately NOT involved — bias never rejects.
    pub fn new(
        config: KvBias,
        monitor: Arc<BackendMonitor>,
        flow_progress: Arc<FlowProgressTracker>,
    ) -> Self {
        Self {
            config,
            monitor,
            flow_progress,
        }
    }

    /// Whether bias is enabled.
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// Current global KV pressure in [0,1], clamped, from the backend monitor.
    pub fn pressure(&self) -> f64 {
        self.monitor
            .snapshot()
            .map(|s| s.kv_usage.clamp(0.0, 1.0))
            .unwrap_or(0.0)
    }

    /// Bias weight in [0,1] for the given pressure.
    ///
    /// 0.0 below `pressure_below` (pure fairness), 1.0 at/above
    /// `bias_full_at` (footprint dominates). Ramp between them.
    pub fn bias_weight(&self, pressure: f64) -> f64 {
        let lo = self.config.pressure_below.max(0.0);
        let hi = self.config.bias_full_at.max(lo);
        if pressure <= lo {
            0.0
        } else if pressure >= hi {
            1.0
        } else {
            (pressure - lo) / (hi - lo)
        }
    }

    /// Delivered-token footprint for a flow (resident KV proxy), 0 if unknown.
    pub fn footprint(&self, flow_id: &crate::flow::FlowId) -> f64 {
        self.flow_progress.delivered_for(flow_id) as f64
    }

    /// Apply the KV bias to candidate selection.
    ///
    /// If bias is disabled, defers to the plain priority selection.
    /// Otherwise each candidate gets a normalized footprint lift weighted by
    /// pressure; the candidate with the highest composite score wins, with
    /// priority, base score, and enqueue time as tie-breaks. When all
    /// footprints are equal (or pressure is 0), this reduces to `select_best`.
    pub fn select(
        &self,
        candidates: &[FlowCandidate],
        pressure: f64,
    ) -> Option<crate::flow::FlowId> {
        let weight = if self.config.enabled {
            self.bias_weight(pressure)
        } else {
            0.0
        };

        if weight <= 0.0 || candidates.is_empty() {
            return crate::scheduler::priority::select_best(candidates);
        }

        // Normalize footprints across this round.
        let max_footprint = candidates
            .iter()
            .map(|c| c.kv_footprint)
            .fold(0.0f64, f64::max);
        if max_footprint <= 0.0 {
            return crate::scheduler::priority::select_best(candidates);
        }

        let mut best: Option<(&FlowCandidate, f64)> = None;
        for cand in candidates {
            let footprint_norm = cand.kv_footprint / max_footprint;
            let score = weight * footprint_norm;
            let better = match best {
                None => true,
                Some((best_cand, best_score)) => {
                    if (score - best_score).abs() > f64::EPSILON {
                        score > best_score
                    } else {
                        // Tie on KV score: fall back to priority, then base,
                        // then enqueue time — identical to select_best order.
                        priority::cmp_fair(cand, best_cand).is_lt()
                    }
                }
            };
            if better {
                best = Some((cand, score));
            }
        }

        best.map(|(c, _)| c.flow_id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::FlowId;
    use std::time::{Duration, Instant};

    fn handle(config: KvBias) -> KvBiasHandle {
        KvBiasHandle::new(
            config,
            Arc::new(BackendMonitor::empty()),
            Arc::new(FlowProgressTracker::new()),
        )
    }

    fn candidate(flow: &str, footprint: f64, enqueued_at: Instant) -> FlowCandidate {
        FlowCandidate {
            flow_id: FlowId::new(flow),
            priority: 50,
            enqueued_at,
            base_score: 0.0,
            kv_footprint: footprint,
        }
    }

    /// Disabled bias: the effective weight is 0 at any pressure, so
    /// selection falls back to pure fairness — the earlier-enqueued
    /// 0-footprint flow beats the 1000-footprint flow even at maximum
    /// pressure. Pinned at the `select` level because `bias_weight` is a
    /// pure pressure mapping and the `enabled` gate lives in `select`.
    #[test]
    fn bias_weight_disabled_zero() {
        let h = handle(KvBias {
            enabled: false,
            ..KvBias::default()
        });
        let t0 = Instant::now();
        let cands = vec![
            candidate("small", 0.0, t0),
            candidate("large", 1000.0, t0 + Duration::from_millis(1)),
        ];
        for pressure in [0.0, 0.5, 0.95] {
            assert_eq!(
                h.select(&cands, pressure),
                Some(FlowId::new("small")),
                "disabled bias must pick the pure-fairness winner at pressure {pressure}"
            );
        }
    }

    /// At full bias strength (pressure >= bias_full_at) the
    /// larger-footprint flow wins regardless of enqueue order.
    #[test]
    fn select_high_pressure_prefers_footprint() {
        let h = handle(KvBias::default());
        let t0 = Instant::now();
        // Fairness alone would pick "small" (earlier enqueue, equal
        // priority and base score); the full-strength bias must override it.
        let cands = vec![
            candidate("small", 0.0, t0),
            candidate("large", 1000.0, t0 + Duration::from_millis(1)),
        ];
        assert_eq!(h.select(&cands, 0.95), Some(FlowId::new("large")));
        // Reversed enqueue order: "large" still wins.
        let cands = vec![
            candidate("large", 1000.0, t0),
            candidate("small", 0.0, t0 + Duration::from_millis(1)),
        ];
        assert_eq!(h.select(&cands, 0.95), Some(FlowId::new("large")));
    }
}
