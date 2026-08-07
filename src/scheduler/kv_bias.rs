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
