//! Per-request lifecycle tracking and credit restoration.
//!
//! `LifecycleGuard` is an RAII handle that bridges the streaming response
//! lifecycle with scheduler accounting. It tracks:
//! - Whether the request stream completed normally (backend finished) or was
//!   aborted (client disconnect, timeout, explicit cancel).
//! - How many tokens were delivered via usage-frame parsing.
//!
//! On Drop, the guard emits the appropriate lifecycle event
//! (`request_completed` or `request_cancelled`) and reports accounting
//! to the scheduler so DRR credit reflects actual delivered work, not
//! just the estimated cost.
//!
//! # Accounting model (DRR)
//!
//! At admission, DRR consumes `credit -= work_unit` (estimated cost).
//! On completion, the guard restores `estimated - actual` so net credit
//! reflects actual delivered tokens. On cancellation, `estimated - delivered`
//! is restored so net credit equals `-delivered` (≈ 0 when nothing was delivered).
//!
//! When usage data is absent, the full estimated cost is consumed (no
//! restore) on completion. This imprecision is acceptable for V1; Phase 3
//! closes it with authoritative token feedback.

use std::cell::Cell;

use crate::flow::FlowId;
use crate::metrics::Metrics;
use crate::scheduler::flow_progress::FlowProgressTracker;
use crate::scheduler::Scheduler;
use std::sync::Arc;

/// Events tracked by the request lifecycle counter.
pub mod event {
    pub const REQUEST_STARTED: &str = "request_started";
    pub const TOKEN_RECEIVED: &str = "token_received";
    pub const REQUEST_COMPLETED: &str = "request_completed";
    pub const REQUEST_CANCELLED: &str = "request_cancelled";
}

/// RAII guard tracking a single request's lifecycle.
///
/// The guard does NOT release the QueueTicket (admission slot) — that is
/// handled independently by the ticket's own drop handler. Instead, the
/// guard tracks whether the request completed normally and reports
/// accounting/credit restoration accordingly.
pub struct LifecycleGuard {
    /// Flow ID for this request.
    flow_id: FlowId,
    /// Estimated cost in tokens (original max_tokens or work_unit).
    estimated_cost: i64,
    /// Scheduler reference for credit accounting.
    scheduler: Arc<Scheduler>,
    /// Prometheus metrics handle for event emission.
    metrics: Arc<Metrics>,
    /// Whether the stream completed normally (backend finished).
    /// Set to true by the stream layer when the backend body ends cleanly.
    completed_normally: Cell<bool>,
    /// Delivered tokens count (from usage-frame parsing).
    /// Updated by the stream layer as usage frames arrive.
    delivered_tokens: Cell<i64>,
    /// Flow progress tracker for predictive admit (optional).
    flow_progress: Option<Arc<FlowProgressTracker>>,
}

impl LifecycleGuard {
    /// Create a new lifecycle guard for a request.
    ///
    /// Emits `request_started` immediately. Registers the flow in the
    /// progress tracker if one is provided.
    pub fn new(
        flow_id: FlowId,
        estimated_cost: i64,
        scheduler: Arc<Scheduler>,
        metrics: Arc<Metrics>,
        flow_progress: Option<Arc<FlowProgressTracker>>,
    ) -> Self {
        metrics
            .request_events_total
            .with_label_values(&[event::REQUEST_STARTED])
            .inc();

        // Register in flow progress tracker (for predictive admit).
        if let Some(ref tracker) = flow_progress {
            tracker.register(&flow_id, estimated_cost);
        }

        Self {
            flow_id,
            estimated_cost,
            scheduler,
            metrics,
            completed_normally: Cell::new(false),
            delivered_tokens: Cell::new(0),
            flow_progress,
        }
    }

    /// Record that tokens were received (for token_received event tracking).
    pub fn record_token(&self) {
        self.metrics
            .request_events_total
            .with_label_values(&[event::TOKEN_RECEIVED])
            .inc();
    }

    /// Update the cumulative delivered tokens count.
    ///
    /// Called by the stream layer each time the TokenAccumulator
    /// extracts tokens from a usage frame. The count is additive.
    /// Also updates the flow progress tracker for predictive admit.
    pub fn add_delivered_tokens(&self, count: i64) {
        let current = self.delivered_tokens.get();
        self.delivered_tokens.set(current + count);
        if let Some(ref tracker) = self.flow_progress {
            tracker.update_delivered(&self.flow_id, count);
        }
    }

    /// Mark the stream as having completed normally (backend body finished).
    ///
    /// Called by the stream layer when the backend stream ends with `None`.
    /// On Drop, if this was NOT called, the guard treats the request as
    /// cancelled and restores the estimated credit.
    pub fn mark_completed(&self) {
        self.completed_normally.set(true);
    }
}

impl Drop for LifecycleGuard {
    fn drop(&mut self) {
        let completed = self.completed_normally.get();
        let delivered = self.delivered_tokens.get();

        // Unregister from flow progress tracker (for predictive admit).
        if let Some(ref tracker) = self.flow_progress {
            tracker.unregister(&self.flow_id, self.estimated_cost, delivered);
        }

        if completed {
            // Normal completion: charge actual delivered, restore the rest.
            self.metrics
                .request_events_total
                .with_label_values(&[event::REQUEST_COMPLETED])
                .inc();

            if delivered > 0 {
                // Restore estimated - delivered so net credit = -delivered.
                // If delivered > estimated (overrun), restore is negative,
                // meaning additional debit for the overrun.
                let restore = self.estimated_cost - delivered;
                self.scheduler.report_accounting(
                    &self.flow_id,
                    AccountingReport::Completed {
                        delivered_tokens: delivered,
                        restore_cost: restore,
                    },
                );
                if restore < 0 {
                    tracing::warn!(
                        flow_id = %self.flow_id,
                        delivered_tokens = delivered,
                        estimated_cost = self.estimated_cost,
                        overrun = -restore,
                        "backend generated more tokens than max_tokens estimate; overrun debit of {} tokens applied",
                        -restore
                    );
                }
            } else {
                // No usage data available — charge full estimated cost.
                tracing::warn!(
                    flow_id = %self.flow_id,
                    estimated_cost = self.estimated_cost,
                    "backend response had no usage data; charging full estimated cost of {} tokens",
                    self.estimated_cost
                );
                self.scheduler.report_accounting(
                    &self.flow_id,
                    AccountingReport::Completed {
                        delivered_tokens: self.estimated_cost,
                        restore_cost: 0,
                    },
                );
            }
        } else {
            // Cancelled (disconnect, timeout, explicit cancel):
            // restore estimated - delivered (the unused remainder).
            self.metrics
                .request_events_total
                .with_label_values(&[event::REQUEST_CANCELLED])
                .inc();

            // On cancel: restore estimated - delivered, so net charge = delivered.
            // saturating_sub guards against delivered > estimated (should not happen).
            let restore = self.estimated_cost.saturating_sub(delivered);
            self.scheduler.report_accounting(
                &self.flow_id,
                AccountingReport::Cancelled {
                    restore_cost: restore,
                },
            );
        }
    }
}

/// Accounting report sent to the scheduler on request completion/cancel.
///
/// DRR uses this to adjust per-flow credit. FIFO/WFQ ignore it.
pub enum AccountingReport {
    /// Request completed: restore the difference between estimated and actual.
    Completed {
        /// Actual tokens delivered (from usage frame).
        delivered_tokens: i64,
        /// Amount to restore: estimated_cost - delivered_tokens.
        restore_cost: i64,
    },
    /// Request cancelled: restore `estimated - delivered` so net credit = `-delivered`.
    Cancelled {
        /// Amount to restore: estimated_cost - delivered_tokens (the unused remainder).
        restore_cost: i64,
    },
}
