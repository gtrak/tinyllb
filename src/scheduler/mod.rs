mod backpressure;
mod fifo;
mod wfq;

pub use backpressure::{fail_fast_retry_after, mode_label, BackpressureRejected};
pub use fifo::{make_ticket, FifoScheduler, QueueTicket};
pub use wfq::WfqScheduler;

use crate::config::Algorithm;
use crate::flow::FlowRegistry;
use crate::metrics::Metrics;
use std::sync::Arc;
use std::time::Duration;

/// Unified scheduler type that dispatches to FIFO or WFQ based on config.
pub enum Scheduler {
    Fifo(FifoScheduler),
    Wfq(WfqScheduler),
}

impl Scheduler {
    /// Create a scheduler based on the configured algorithm.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        algorithm: Algorithm,
        max_active_flows: u32,
        metrics: Arc<Metrics>,
        registry: Arc<FlowRegistry>,
        backpressure_mode: crate::config::BackpressureMode,
        max_queue_depth: u32,
        max_wait: Duration,
        retry_after_base: Duration,
    ) -> Self {
        match algorithm {
            Algorithm::Fifo => Self::Fifo(FifoScheduler::new(
                max_active_flows,
                metrics,
                registry,
                backpressure_mode,
                max_queue_depth,
                max_wait,
                retry_after_base,
            )),
            Algorithm::Wfq => Self::Wfq(WfqScheduler::new(
                max_active_flows,
                metrics,
                registry,
                backpressure_mode,
                max_queue_depth,
                max_wait,
                retry_after_base,
            )),
            Algorithm::Drr => {
                // DRR not yet implemented; fall back to WFQ.
                tracing::warn!("DRR algorithm not yet implemented, falling back to WFQ");
                Self::Wfq(WfqScheduler::new(
                    max_active_flows,
                    metrics,
                    registry,
                    backpressure_mode,
                    max_queue_depth,
                    max_wait,
                    retry_after_base,
                ))
            }
        }
    }

    /// Attempt to admit a request into the active set.
    pub async fn admit(
        &self,
        flow_id: crate::flow::FlowId,
        work_unit: f64,
    ) -> Result<QueueTicket, BackpressureRejected> {
        match self {
            Self::Fifo(s) => s.admit(flow_id, work_unit).await,
            Self::Wfq(s) => s.admit(flow_id, work_unit).await,
        }
    }

    /// Current number of requests waiting in the queue.
    pub fn queue_depth(&self) -> u32 {
        match self {
            Self::Fifo(s) => s.queue_depth(),
            Self::Wfq(s) => s.queue_depth(),
        }
    }

    /// Build a snapshot of the current queue state.
    pub fn queue_snapshot(&self) -> crate::flow::QueueSnapshot {
        match self {
            Self::Fifo(s) => s.queue_snapshot(),
            Self::Wfq(s) => s.queue_snapshot(),
        }
    }

    /// Return the total service_done for the given flow (WFQ only).
    /// For FIFO this always returns 0.0.
    pub fn service_done(&self, flow_id: &crate::flow::FlowId) -> f64 {
        match self {
            Self::Fifo(_) => 0.0,
            Self::Wfq(s) => s.service_done(flow_id),
        }
    }
}
