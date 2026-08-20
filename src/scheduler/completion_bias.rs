//! Completion bias gate.
//!
//! Defers admission of requests for *new* flows (flows without in-flight
//! requests) while `active_flows >= target_active_flows`.  Active flows
//! (already admitted) bypass the gate entirely so a flow keeps its slot for
//! back-to-back requests.
//!
//! Starvation protection takes precedence over completion bias: if a flow has
//! been waiting longer than `starvation_timeout`, the gate allows it through
//! immediately and records starvation metrics.
//!
//! When `predictive_admit` is enabled, the gate also checks per-flow token
//! progress: if any active flow has delivered >= 90% of its estimated tokens,
//! the gate allows a new flow through (predictive admit) before the active
//! flow finishes.

use std::sync::Arc;
use std::time::Duration;

use crate::flow::{Flow, FlowRegistry};
use crate::metrics::Metrics;
use crate::scheduler::flow_progress::FlowProgressTracker;
use crate::scheduler::starvation;

/// Threshold for predictive admit: if delivered >= this fraction of estimated,
/// the flow is considered "near done."
const PREDICTIVE_ADMIT_THRESHOLD: f64 = 0.9;

/// Pre-admission gate that enforces completion bias.
// @lat: [[scheduler_policies#Completion Bias Gate]]
pub struct CompletionBiasGate {
    /// Whether completion bias is enabled.
    enabled: bool,
    /// Target number of active flows.  When `active >= target`, new flows wait.
    target_active_flows: u32,
    /// Whether predictive admit is enabled (allow pre-admit when a flow is near done).
    predictive_admit: bool,
    /// Metrics handle for recording starvation events.
    metrics: Arc<Metrics>,
    /// Shared flow registry (reserved for future use).
    #[allow(dead_code)]
    registry: Arc<FlowRegistry>,
    /// Notify the gate that active flows may have changed (e.g. a ticket dropped).
    notify: Arc<tokio::sync::Notify>,
    /// Time to re-check starvation while waiting.
    starvation_check_interval: Duration,
    /// Starvation timeout: if exceeded, force the flow through.
    starvation_timeout: Duration,
    /// Flow progress tracker for predictive admit.
    flow_progress: Arc<FlowProgressTracker>,
}

impl CompletionBiasGate {
    /// Create a new completion bias gate.
    ///
    /// `target_active_flows` of `0` means "use `max_active_flows`".
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        enabled: bool,
        target_active_flows: u32,
        predictive_admit: bool,
        max_active_flows: u32,
        metrics: Arc<Metrics>,
        registry: Arc<FlowRegistry>,
        notify: Arc<tokio::sync::Notify>,
        starvation_timeout: Duration,
        flow_progress: Arc<FlowProgressTracker>,
    ) -> Self {
        let effective_target = if target_active_flows == 0 {
            max_active_flows
        } else {
            target_active_flows
        };
        Self {
            enabled,
            target_active_flows: effective_target,
            predictive_admit,
            metrics,
            registry,
            notify,
            starvation_check_interval: starvation_timeout / 4,
            starvation_timeout,
            flow_progress,
        }
    }

    /// Check if a request for `flow` should be admitted immediately, or if it
    /// should wait for active flows to drop below the target.
    ///
    /// - If the flow is already active, always admit (active flows keep their slot).
    /// - If active flows < target, always admit.
    /// - If predictive admit is ON and any active flow is near done (>= 90% delivered),
    ///   admit immediately (predictive admit).
    /// - If the flow has been waiting longer than `starvation_timeout`, force
    ///   admit and record starvation metrics.
    /// - Otherwise, wait until active flows < target, re-checking periodically
    ///   for starvation and predictive admit.
    pub async fn check(&self, flow: &Arc<Flow>) {
        // Not enabled — always proceed.
        if !self.enabled {
            return;
        }

        // Already-active flows bypass the gate.
        if flow.is_active() {
            return;
        }

        // If target is 0 (e.g. max_active_flows=0), nothing to wait for
        // — defer to the backpressure mechanism.
        if self.target_active_flows == 0 {
            return;
        }

        // If under target, always proceed.
        let active = self.metrics.active_flows.get() as u32;
        if active < self.target_active_flows {
            return;
        }

        // At or above target. Check predictive admit before waiting.
        if self.predictive_admit
            && self
                .flow_progress
                .any_flow_near_done(PREDICTIVE_ADMIT_THRESHOLD)
        {
            // An active flow is near done — allow the new flow through early.
            return;
        }

        // Wait for a slot to free, checking for starvation and predictive admit.
        loop {
            // Check starvation: if this flow has been waiting too long, force through.
            if self.maybe_force_admit(flow).await {
                return;
            }

            // Predictive admit: if any active flow is near done, allow through.
            if self.predictive_admit
                && self
                    .flow_progress
                    .any_flow_near_done(PREDICTIVE_ADMIT_THRESHOLD)
            {
                return;
            }

            // Wait for a notification (slot freed) with a timeout to re-check.
            let notified = self.notify.notified();
            let _ = tokio::time::timeout(self.starvation_check_interval, notified).await;

            // Re-check conditions after waking.
            let active = self.metrics.active_flows.get() as u32;
            if active < self.target_active_flows {
                return;
            }
        }
    }

    /// Check if the flow has exceeded the starvation timeout.
    /// If so, record metrics and return true to allow the flow through.
    async fn maybe_force_admit(&self, flow: &Arc<Flow>) -> bool {
        if let Some(wait) = starvation::is_starved(flow, self.starvation_timeout) {
            starvation::record_force_admit(&self.metrics, flow, wait);
            true
        } else {
            false
        }
    }

    /// Wake all waiters.  Called when a ticket is dropped (active flow count changes).
    pub fn notify_waiters(&self) {
        self.notify.notify_waiters();
    }
}
