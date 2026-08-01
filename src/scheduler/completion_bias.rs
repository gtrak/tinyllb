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

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::flow::{Flow, FlowRegistry};
use crate::metrics::Metrics;

/// Pre-admission gate that enforces completion bias.
pub struct CompletionBiasGate {
    /// Whether completion bias is enabled.
    enabled: bool,
    /// Target number of active flows.  When `active >= target`, new flows wait.
    target_active_flows: u32,
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
}

impl CompletionBiasGate {
    /// Create a new completion bias gate.
    ///
    /// `target_active_flows` of `0` means "use `max_active_flows`".
    pub fn new(
        enabled: bool,
        target_active_flows: u32,
        max_active_flows: u32,
        metrics: Arc<Metrics>,
        registry: Arc<FlowRegistry>,
        notify: Arc<tokio::sync::Notify>,
        starvation_timeout: Duration,
    ) -> Self {
        let effective_target = if target_active_flows == 0 {
            max_active_flows
        } else {
            target_active_flows
        };
        Self {
            enabled,
            target_active_flows: effective_target,
            metrics,
            registry,
            notify,
            starvation_check_interval: starvation_timeout / 4,
            starvation_timeout,
        }
    }

    /// Check if a request for `flow` should be admitted immediately, or if it
    /// should wait for active flows to drop below the target.
    ///
    /// - If the flow is already active, always admit (active flows keep their slot).
    /// - If active flows < target, always admit.
    /// - If the flow has been waiting longer than `starvation_timeout`, force
    ///   admit and record starvation metrics.
    /// - Otherwise, wait until active flows < target, re-checking periodically
    ///   for starvation.
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

        // Wait for a slot to free, checking for starvation.
        loop {
            // Check starvation: if this flow has been waiting too long, force through.
            if self.maybe_force_admit(flow).await {
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
        let enqueued_at = flow.enqueued_at.read().unwrap();
        if let Some(queued_at) = *enqueued_at {
            let wait = Instant::now().duration_since(queued_at);
            if wait > self.starvation_timeout {
                self.metrics
                    .flow_starvation_seconds
                    .with_label_values(&[flow.id.metric_label()])
                    .set(wait.as_secs_f64());
                self.metrics.starvation_force_admits_total.inc();
                return true;
            }
        }
        false
    }

    /// Wake all waiters.  Called when a ticket is dropped (active flow count changes).
    pub fn notify_waiters(&self) {
        self.notify.notify_waiters();
    }
}
