//! Per-flow token progress tracking for predictive admit.

//! Tracks aggregate estimated and delivered tokens across active requests
//! per flow. Used by the completion bias gate to decide whether an active
//! flow is "near done" (delivered >= threshold * estimated), allowing
//! predictive pre-admit of a new flow.

use dashmap::DashMap;

/// Per-flow progress entry.
#[derive(Debug)]
struct ProgressEntry {
    /// Total estimated tokens for all active requests of this flow.
    estimated: i64,
    /// Total delivered tokens for all active requests of this flow.
    delivered: i64,
}

/// Thread-safe tracker of per-flow token progress.
///
/// Updated by LifecycleGuard (or the stream layer) when requests start,
/// deliver tokens, or end. The completion bias gate queries this to
/// decide whether any active flow is "near done."
pub struct FlowProgressTracker {
    entries: DashMap<crate::flow::FlowId, ProgressEntry>,
}

impl FlowProgressTracker {
    /// Create a new empty tracker.
    pub fn new() -> Self {
        Self {
            entries: DashMap::new(),
        }
    }

    /// Register a new active request for a flow.
    ///
    /// Called when a request is admitted. Adds `estimated` to the flow's
    /// aggregate estimated tokens.
    pub fn register(&self, flow_id: &crate::flow::FlowId, estimated: i64) {
        let mut entry = self
            .entries
            .entry(flow_id.clone())
            .or_insert_with(|| ProgressEntry {
                estimated: 0,
                delivered: 0,
            });
        entry.estimated += estimated;
    }

    /// Update the delivered token count for a flow.
    ///
    /// Called when tokens are delivered (from usage frame parsing).
    /// Increments the flow's aggregate delivered by `delta`.
    pub fn update_delivered(&self, flow_id: &crate::flow::FlowId, delta: i64) {
        if let Some(mut entry) = self.entries.get_mut(flow_id) {
            entry.delivered += delta;
        }
    }

    /// Unregister a completed or cancelled request.
    ///
    /// Subtracts the request's estimated and delivered from the flow's
    /// aggregates. If the flow has no more active requests, the entry
    /// is removed entirely.
    pub fn unregister(&self, flow_id: &crate::flow::FlowId, estimated: i64, delivered: i64) {
        if let Some(mut entry) = self.entries.get_mut(flow_id) {
            let new_est = entry.estimated.saturating_sub(estimated);
            let new_del = entry.delivered.saturating_sub(delivered);
            if new_est == 0 && new_del == 0 {
                drop(entry);
                self.entries.remove(flow_id);
            } else {
                entry.estimated = new_est;
                entry.delivered = new_del;
            }
        }
    }

    /// Check if a specific flow is near done (delivered >= threshold * estimated).
    ///
    /// Returns `true` if the flow's delivered tokens meet or exceed the
    /// threshold fraction of estimated tokens. Returns `false` if the
    /// flow has no active requests or the threshold is not met.
    pub fn is_near_done(&self, flow_id: &crate::flow::FlowId, threshold: f64) -> bool {
        if let Some(entry) = self.entries.get(flow_id) {
            let est = entry.estimated;
            let del = entry.delivered;
            if est > 0 {
                let ratio = del as f64 / est as f64;
                ratio >= threshold
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Check if any tracked flow is near done.
    ///
    /// Returns `true` if any flow in the tracker has delivered tokens
    /// meeting or exceeding the threshold fraction of estimated tokens.
    pub fn any_flow_near_done(&self, threshold: f64) -> bool {
        for entry in self.entries.iter() {
            let est = entry.estimated;
            let del = entry.delivered;
            if est > 0 {
                let ratio = del as f64 / est as f64;
                if ratio >= threshold {
                    return true;
                }
            }
        }
        false
    }
}

impl Default for FlowProgressTracker {
    fn default() -> Self {
        Self::new()
    }
}
