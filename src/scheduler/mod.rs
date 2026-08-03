mod backpressure;
mod completion_bias;
mod drr;
mod fifo;
mod flow_progress;
mod kv_admission;
pub mod lifecycle;
mod priority;
mod starvation;
mod wfq;

pub use backpressure::{fail_fast_retry_after, mode_label, BackpressureRejected};
pub use drr::DrrScheduler;
pub use fifo::{make_ticket, FifoScheduler, QueueTicket};
pub use flow_progress::FlowProgressTracker;
pub use kv_admission::KvPolicy;
pub use lifecycle::AccountingReport;
pub use wfq::WfqScheduler;

use crate::backend::BackendMonitor;
use crate::config::Algorithm;
use crate::config::CompletionBias;
use crate::config::KvPolicyConfig;
use crate::flow::FlowRegistry;
use crate::metrics::Metrics;
use std::sync::Arc;
use std::time::Duration;

/// Shared policy state for all scheduler variants.
///
/// Each scheduler type gets an Arc clone of the completion bias gate,
/// the starvation timeout, and the flow progress tracker.
#[allow(dead_code)]
pub(crate) struct Policies {
    /// Completion bias gate for checking before admit.
    completion_bias: Arc<completion_bias::CompletionBiasGate>,
    /// Starvation timeout for force-admit in try_select.
    starvation_timeout: Duration,
    /// Notify completion bias waiters when active flows change.
    notify: Arc<tokio::sync::Notify>,
    /// Flow progress tracker for predictive admit.
    flow_progress: Arc<flow_progress::FlowProgressTracker>,
}

impl Policies {
    fn new(
        completion_bias: CompletionBias,
        max_active_flows: u32,
        starvation_timeout: Duration,
        metrics: Arc<Metrics>,
        registry: Arc<FlowRegistry>,
        notify: Arc<tokio::sync::Notify>,
        flow_progress: Arc<flow_progress::FlowProgressTracker>,
    ) -> Self {
        let gate = completion_bias::CompletionBiasGate::new(
            completion_bias.enabled,
            completion_bias.target_active_flows,
            completion_bias.predictive_admit,
            max_active_flows,
            metrics,
            registry,
            notify.clone(),
            starvation_timeout,
            flow_progress.clone(),
        );
        Self {
            completion_bias: Arc::new(gate),
            starvation_timeout,
            notify,
            flow_progress,
        }
    }
}

/// Internal enum for the scheduler algorithm implementation.
enum SchedulerImpl {
    Fifo(FifoScheduler),
    Wfq(WfqScheduler),
    Drr(DrrScheduler),
}

/// Unified scheduler type that dispatches to FIFO, WFQ, or DRR based on config.
///
/// Wraps a shared `KvPolicy` gate that runs before the flow scheduler,
/// enabling KV-cache-aware admission decisions.
// @lat: [[scheduler#Scheduler Facade and Policy Selection]]
pub struct Scheduler {
    /// The underlying scheduling algorithm.
    inner: SchedulerImpl,
    /// KV-cache-aware admission gate.  Checked before every admit.
    kv_policy: Arc<KvPolicy>,
    /// Flow progress tracker for predictive admit.
    flow_progress: Arc<flow_progress::FlowProgressTracker>,
    /// Human-readable algorithm name for tracing.
    algorithm_label: &'static str,
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
        kv_config: KvPolicyConfig,
        monitor: Arc<BackendMonitor>,
    ) -> Self {
        let notify = Arc::new(tokio::sync::Notify::new());
        let flow_progress = Arc::new(flow_progress::FlowProgressTracker::new());
        let policies = Policies::new(
            completion_bias,
            max_active_flows,
            starvation_timeout,
            metrics.clone(),
            registry.clone(),
            notify.clone(),
            flow_progress.clone(),
        );

        let kv_policy = Arc::new(KvPolicy::new(
            &kv_config,
            monitor,
            metrics.clone(),
            backpressure_mode,
            max_wait,
            retry_after_base,
            max_queue_depth,
        ));

        let algorithm_label = match algorithm {
            Algorithm::Fifo => "fifo",
            Algorithm::Wfq => "wfq",
            Algorithm::Drr => "drr",
        };

        let inner = match algorithm {
            Algorithm::Fifo => SchedulerImpl::Fifo(FifoScheduler::new_with_policies(
                max_active_flows,
                metrics,
                registry,
                backpressure_mode,
                max_queue_depth,
                max_wait,
                retry_after_base,
                policies,
            )),
            Algorithm::Wfq => SchedulerImpl::Wfq(WfqScheduler::new_with_policies(
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
            Algorithm::Drr => SchedulerImpl::Drr(DrrScheduler::new_with_policies(
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
        };

        Self {
            kv_policy,
            inner,
            flow_progress,
            algorithm_label,
        }
    }

    /// Create a scheduler with default policy values.
    ///
    /// Backward-compatible constructor for existing test code.  Uses:
    /// - `starvation_timeout = 300s` (effectively disabled for short tests)
    /// - `completion_bias = default` (enabled, target = max_active_flows)
    /// - `kv_policy = disabled` (enabled=false)
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
        let monitor = Arc::new(BackendMonitor::empty());
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
            KvPolicyConfig::default(),
            monitor,
        )
    }

    /// Attempt to admit a request into the active set.
    ///
    /// KV policy runs first (accept/delay/reject based on KV-cache pressure).
    /// If KV policy accepts, delegates to the underlying flow scheduler.
    #[tracing::instrument(skip(self, flow_id, work_unit), fields(
        flow_id = %flow_id,
        queue_depth_before,
        algorithm = self.algorithm_label,
    ))]
    pub async fn admit(
        &self,
        flow_id: crate::flow::FlowId,
        work_unit: f64,
    ) -> Result<QueueTicket, BackpressureRejected> {
        // Record queue depth before the admit process begins.
        tracing::Span::current().record("queue_depth_before", self.queue_depth());

        let enter = std::time::Instant::now();

        // KV policy gate runs FIRST before any flow scheduling.
        self.kv_policy.check().await?;
        let result = match &self.inner {
            SchedulerImpl::Fifo(s) => s.admit(flow_id, work_unit).await,
            SchedulerImpl::Wfq(s) => s.admit(flow_id, work_unit).await,
            SchedulerImpl::Drr(s) => s.admit(flow_id, work_unit).await,
        };

        // Emit terminal decision event (accept or reject) inside the admit span.
        let wait_secs = enter.elapsed().as_secs_f64();
        match &result {
            Ok(_) => {
                tracing::info!(
                    decision = "accept",
                    wait_seconds = wait_secs,
                    "admit decision"
                );
            }
            Err(_) => {
                tracing::info!(
                    decision = "reject",
                    wait_seconds = wait_secs,
                    "admit decision"
                );
            }
        }

        result
    }

    /// Current number of requests waiting in the queue.
    ///
    /// Includes both flow-scheduler queue depth and KV-delay-waiting requests.
    pub fn queue_depth(&self) -> u32 {
        let inner_depth = match &self.inner {
            SchedulerImpl::Fifo(s) => s.queue_depth(),
            SchedulerImpl::Wfq(s) => s.queue_depth(),
            SchedulerImpl::Drr(s) => s.queue_depth(),
        };
        inner_depth + self.kv_policy.delayed_count()
    }

    /// Build a snapshot of the current queue state.
    ///
    /// Waiting count includes both flow-scheduler queue depth and KV-delay
    /// requests so GET /queue reports all pending requests.
    pub fn queue_snapshot(&self) -> crate::flow::QueueSnapshot {
        let inner_snapshot = match &self.inner {
            SchedulerImpl::Fifo(s) => s.queue_snapshot(),
            SchedulerImpl::Wfq(s) => s.queue_snapshot(),
            SchedulerImpl::Drr(s) => s.queue_snapshot(),
        };
        // Add delayed count to the waiting total.
        let delayed = self.kv_policy.delayed_count();
        crate::flow::QueueSnapshot {
            active: inner_snapshot.active,
            waiting: inner_snapshot.waiting + delayed as u64,
            flows: inner_snapshot.flows,
        }
    }

    /// Return the total service_done for the given flow (WFQ only).
    /// For FIFO this always returns 0.0.
    pub fn service_done(&self, flow_id: &crate::flow::FlowId) -> f64 {
        match &self.inner {
            SchedulerImpl::Fifo(_) => 0.0,
            SchedulerImpl::Wfq(s) => s.service_done(flow_id),
            SchedulerImpl::Drr(_) => 0.0,
        }
    }

    /// Return the current credit for the given flow (DRR only).
    /// For FIFO and WFQ this always returns 0.
    pub fn credit(&self, flow_id: &crate::flow::FlowId) -> i64 {
        match &self.inner {
            SchedulerImpl::Fifo(_) => 0,
            SchedulerImpl::Wfq(_) => 0,
            SchedulerImpl::Drr(s) => s.credit(flow_id),
        }
    }

    /// Report accounting for a completed or cancelled request.
    ///
    /// DRR adjusts per-flow credit based on actual delivered tokens.
    /// FIFO and WFQ are no-ops (they don't use per-request credit).
    pub fn report_accounting(&self, flow_id: &crate::flow::FlowId, report: AccountingReport) {
        match &self.inner {
            SchedulerImpl::Fifo(_) => {}
            SchedulerImpl::Wfq(_) => {}
            SchedulerImpl::Drr(s) => s.report_accounting(flow_id, report),
        }
    }

    /// Return a reference to the flow progress tracker for predictive admit.
    pub fn flow_progress_tracker(&self) -> Arc<flow_progress::FlowProgressTracker> {
        self.flow_progress.clone()
    }
}
