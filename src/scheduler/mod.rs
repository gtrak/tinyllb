mod backpressure;
mod completion_bias;
mod drr;
mod fifo;
pub mod lifecycle;
mod priority;
mod starvation;
mod wfq;

pub use backpressure::{fail_fast_retry_after, mode_label, BackpressureRejected};
pub use drr::DrrScheduler;
pub use fifo::{make_ticket, FifoScheduler, QueueTicket};
pub use lifecycle::AccountingReport;
pub use wfq::WfqScheduler;

use crate::config::Algorithm;
use crate::config::CompletionBias;
use crate::flow::FlowRegistry;
use crate::metrics::Metrics;
use std::sync::Arc;
use std::time::Duration;

/// Shared policy state for all scheduler variants.
///
/// Each scheduler type gets an Arc clone of the completion bias gate and
/// the starvation timeout.
#[allow(dead_code)]
pub(crate) struct Policies {
    /// Completion bias gate for checking before admit.
    completion_bias: Arc<completion_bias::CompletionBiasGate>,
    /// Starvation timeout for force-admit in try_select.
    starvation_timeout: Duration,
    /// Notify completion bias waiters when active flows change.
    notify: Arc<tokio::sync::Notify>,
}

impl Policies {
    fn new(
        completion_bias: CompletionBias,
        max_active_flows: u32,
        starvation_timeout: Duration,
        metrics: Arc<Metrics>,
        registry: Arc<FlowRegistry>,
        notify: Arc<tokio::sync::Notify>,
    ) -> Self {
        let gate = completion_bias::CompletionBiasGate::new(
            completion_bias.enabled,
            completion_bias.target_active_flows,
            max_active_flows,
            metrics,
            registry,
            notify.clone(),
            starvation_timeout,
        );
        Self {
            completion_bias: Arc::new(gate),
            starvation_timeout,
            notify,
        }
    }
}

/// Unified scheduler type that dispatches to FIFO, WFQ, or DRR based on config.
pub enum Scheduler {
    Fifo(FifoScheduler),
    Wfq(WfqScheduler),
    Drr(DrrScheduler),
}

impl Scheduler {
    /// Create a scheduler based on the configured algorithm.
    ///
    /// This is the full constructor that accepts all policy parameters.
    /// Use [`Scheduler::new_with_defaults`](Self::new_with_defaults) for
    /// backward-compatible construction with default policy values.
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
        starvation_timeout: Duration,
        completion_bias: CompletionBias,
    ) -> Self {
        let notify = Arc::new(tokio::sync::Notify::new());
        let policies = Policies::new(
            completion_bias,
            max_active_flows,
            starvation_timeout,
            metrics.clone(),
            registry.clone(),
            notify.clone(),
        );

        match algorithm {
            Algorithm::Fifo => Self::Fifo(FifoScheduler::new_with_policies(
                max_active_flows,
                metrics,
                registry,
                backpressure_mode,
                max_queue_depth,
                max_wait,
                retry_after_base,
                policies,
            )),
            Algorithm::Wfq => Self::Wfq(WfqScheduler::new_with_policies(
                max_active_flows,
                metrics,
                registry,
                backpressure_mode,
                max_queue_depth,
                max_wait,
                retry_after_base,
                starvation_timeout,
                policies,
            )),
            Algorithm::Drr => Self::Drr(DrrScheduler::new_with_policies(
                max_active_flows,
                metrics,
                registry,
                backpressure_mode,
                max_queue_depth,
                max_wait,
                retry_after_base,
                starvation_timeout,
                policies,
            )),
        }
    }

    /// Create a scheduler with default policy values.
    ///
    /// Backward-compatible constructor for existing test code.  Uses:
    /// - `starvation_timeout = 300s` (effectively disabled for short tests)
    /// - `completion_bias = default` (enabled, target = max_active_flows)
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_defaults(
        algorithm: Algorithm,
        max_active_flows: u32,
        metrics: Arc<Metrics>,
        registry: Arc<FlowRegistry>,
        backpressure_mode: crate::config::BackpressureMode,
        max_queue_depth: u32,
        max_wait: Duration,
        retry_after_base: Duration,
    ) -> Self {
        Self::new(
            algorithm,
            max_active_flows,
            metrics,
            registry,
            backpressure_mode,
            max_queue_depth,
            max_wait,
            retry_after_base,
            Duration::from_secs(300),
            CompletionBias::default(),
        )
    }

    /// Attempt to admit a request into the active set.
    ///
    /// Applies completion bias before delegating to the underlying scheduler.
    pub async fn admit(
        &self,
        flow_id: crate::flow::FlowId,
        work_unit: f64,
    ) -> Result<QueueTicket, BackpressureRejected> {
        match self {
            Self::Fifo(s) => s.admit(flow_id, work_unit).await,
            Self::Wfq(s) => s.admit(flow_id, work_unit).await,
            Self::Drr(s) => s.admit(flow_id, work_unit).await,
        }
    }

    /// Current number of requests waiting in the queue.
    pub fn queue_depth(&self) -> u32 {
        match self {
            Self::Fifo(s) => s.queue_depth(),
            Self::Wfq(s) => s.queue_depth(),
            Self::Drr(s) => s.queue_depth(),
        }
    }

    /// Build a snapshot of the current queue state.
    pub fn queue_snapshot(&self) -> crate::flow::QueueSnapshot {
        match self {
            Self::Fifo(s) => s.queue_snapshot(),
            Self::Wfq(s) => s.queue_snapshot(),
            Self::Drr(s) => s.queue_snapshot(),
        }
    }

    /// Return the total service_done for the given flow (WFQ only).
    /// For FIFO this always returns 0.0.
    pub fn service_done(&self, flow_id: &crate::flow::FlowId) -> f64 {
        match self {
            Self::Fifo(_) => 0.0,
            Self::Wfq(s) => s.service_done(flow_id),
            Self::Drr(_) => 0.0,
        }
    }

    /// Return the current credit for the given flow (DRR only).
    /// For FIFO and WFQ this always returns 0.
    pub fn credit(&self, flow_id: &crate::flow::FlowId) -> i64 {
        match self {
            Self::Fifo(_) => 0,
            Self::Wfq(_) => 0,
            Self::Drr(s) => s.credit(flow_id),
        }
    }

    /// Report accounting for a completed or cancelled request.
    ///
    /// DRR adjusts per-flow credit based on actual delivered tokens.
    /// FIFO and WFQ are no-ops (they don't use per-request credit).
    pub fn report_accounting(&self, flow_id: &crate::flow::FlowId, report: AccountingReport) {
        match self {
            Self::Fifo(_) => {}
            Self::Wfq(_) => {}
            Self::Drr(s) => s.report_accounting(flow_id, report),
        }
    }
}
