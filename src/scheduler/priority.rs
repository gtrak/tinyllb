//! Priority-aware flow selection helpers.
//!
//! Among eligible waiting flows, the one with the highest `priority` value is
//! preferred.  Ties are broken by the base algorithm's rule (min ratio for WFQ,
//! RR order for DRR).
//!
//! This module provides the core priority selection logic used by each
//! scheduler's `try_select` to integrate priority without duplicating code.

use std::time::Instant;

use crate::flow::FlowId;

/// A candidate flow for selection.
pub struct FlowCandidate {
    /// The flow's unique ID.
    pub flow_id: FlowId,
    /// The flow's priority (higher = more urgent).
    pub priority: u32,
    /// When this flow was enqueued (for FIFO tie-breaking).
    pub enqueued_at: Instant,
    /// Base algorithm tiebreak value (lower = preferred).
    /// For WFQ this is the `service_done / weight` ratio.
    /// For DRR this is the round-robin cursor index.
    pub base_score: f64,
}

/// Select the best flow from candidates using priority as the primary sort key.
///
/// Selection order:
/// 1. Highest `priority` wins.
/// 2. Ties broken by lowest `base_score` (base algorithm preference).
/// 3. Further ties broken by earliest `enqueued_at` (FIFO).
///
/// Returns the flow_id of the selected candidate, or `None` if no candidates.
// @lat: [[scheduler_policies#Priority-Aware Flow Selection]]
pub fn select_best(candidates: &[FlowCandidate]) -> Option<FlowId> {
    let mut best: Option<&FlowCandidate> = None;

    for cand in candidates {
        match best {
            None => {
                best = Some(cand);
            }
            Some(current_best) => {
                // Priority is the primary sort key (higher wins).
                if cand.priority > current_best.priority {
                    best = Some(cand);
                } else if cand.priority == current_best.priority {
                    // Tie-break by base algorithm score (lower wins).
                    if cand.base_score < current_best.base_score {
                        best = Some(cand);
                    } else if (cand.base_score - current_best.base_score).abs() < f64::EPSILON {
                        // Further tie-break by enqueue time (earlier wins).
                        if cand.enqueued_at < current_best.enqueued_at {
                            best = Some(cand);
                        }
                    }
                }
                // else: current_best has higher priority, keep it.
            }
        }
    }

    best.map(|c| c.flow_id.clone())
}
