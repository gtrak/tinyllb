mod backpressure;
mod completion_bias;
mod drr;
mod flow_progress;
mod kv_admission;
mod kv_bias;
pub mod lifecycle;
mod priority;
mod starvation;
pub mod ticket;

pub use backpressure::{fail_fast_retry_after, mode_label, BackpressureRejected};
pub use drr::DrrScheduler;
pub use flow_progress::FlowProgressTracker;
pub use ticket::{make_ticket, QueueTicket};
pub use kv_admission::KvPolicy;
pub use kv_bias::KvBiasHandle;
pub use lifecycle::AccountingReport;

use crate::backend::BackendMonitor;
use crate::config::CompletionBias;
use crate::config::KvBias;
use crate::config::KvPolicyConfig;
use crate::config::Priorities;
use crate::config::PriorityPolicy;
use crate::flow::cadence::CadenceRegistry;
use crate::flow::FlowRegistry;
use crate::metrics::Metrics;
use std::sync::Arc;
use std::time::Duration;

/// Unified scheduler facade wrapping the DRR flow scheduler.
///
/// Wraps a shared `KvPolicy` gate that runs before the flow scheduler,
/// enabling KV-cache-aware admission decisions.
// @lat: [[scheduler#Scheduler Facade and Policy Selection]]
pub struct Scheduler {
    /// The DRR flow scheduler.
    inner: DrrScheduler,
    /// KV-cache-aware admission gate.  Checked before every admit.
    kv_policy: Arc<KvPolicy>,
    /// Flow progress tracker for predictive admit.
    flow_progress: Arc<flow_progress::FlowProgressTracker>,
    /// Flow registry (lifted for cadence lookup in `admit`).
    registry: Arc<FlowRegistry>,
    /// Cadence heuristic registry for interactive-vs-batch classification.
    cadence: Arc<CadenceRegistry>,
    /// Metrics collector for priority heuristic observability.
    metrics: Arc<Metrics>,
    /// Stall signal from the backend monitor (`true` = engine stalled).
    /// While set, new admissions are rejected with 429 + Retry-After.
    stall_rx: tokio::sync::watch::Receiver<bool>,
}

impl Scheduler {
    /// Create a scheduler.
    ///
    /// This is the full constructor that accepts all policy parameters.
    /// Use [`Scheduler::new_with_defaults`](Self::new_with_defaults) for
    /// backward-compatible construction with default policy values.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
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
        priority_policy: PriorityPolicy,
        priorities: Priorities,
        kv_bias: KvBias,
    ) -> Self {
        let notify = Arc::new(tokio::sync::Notify::new());
        let stall_rx = monitor.stall_receiver();
        let flow_progress = Arc::new(flow_progress::FlowProgressTracker::new());
        let gate = Arc::new(completion_bias::CompletionBiasGate::new(
            completion_bias.enabled,
            completion_bias.target_active_flows,
            completion_bias.predictive_admit,
            max_active_flows,
            metrics.clone(),
            registry.clone(),
            notify.clone(),
            starvation_timeout,
            flow_progress.clone(),
        ));
        let kv_bias_handle = Arc::new(kv_bias::KvBiasHandle::new(
            kv_bias,
            monitor.clone(),
            flow_progress.clone(),
        ));

        let kv_policy = Arc::new(KvPolicy::new(
            &kv_config,
            monitor,
            metrics.clone(),
            backpressure_mode,
            max_wait,
            retry_after_base,
            max_queue_depth,
        ));

        let inner = DrrScheduler::new_with_policies(
            max_active_flows,
            metrics.clone(),
            registry.clone(),
            backpressure_mode,
            max_queue_depth,
            max_wait,
            retry_after_base,
            starvation_timeout,
            gate,
            kv_bias_handle,
        );

        let cadence = Arc::new(CadenceRegistry::new(
            Arc::new(priority_policy),
            Arc::new(priorities),
        ));

        Self {
            kv_policy,
            inner,
            flow_progress,
            registry,
            cadence,
            metrics,
            stall_rx,
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
            PriorityPolicy::default(),
            Priorities::default(),
            KvBias::default(),
        )
    }

    /// Attempt to admit a request into the active set.
    ///
    /// Backward-compatible wrapper: defaults `is_turn_boundary = true`
    /// (optimistic). All existing test/bench callers use this. The proxy
    /// handler uses `admit_with_turn_boundary` to pass the detected value.
    #[tracing::instrument(skip(self, flow_id, work_unit), fields(
        flow_id = %flow_id,
        queue_depth_before,
        algorithm = "drr",
    ))]
    pub async fn admit(
        &self,
        flow_id: crate::flow::FlowId,
        work_unit: f64,
    ) -> Result<QueueTicket, BackpressureRejected> {
        self.admit_with_turn_boundary(flow_id, work_unit, true).await
    }

    /// Admit with an explicit turn-boundary flag.
    ///
    /// `is_turn_boundary = true` means the current request's last message has
    /// `role: "user"` (or non-chat / optimistic). `false` means `role: "tool"`
    /// or `"assistant"` (intra-turn continuation). The cadence state machine
    /// uses this to distinguish turn-boundary idles from tool-execution gaps.
    #[tracing::instrument(skip(self, flow_id, work_unit), fields(
        flow_id = %flow_id,
        queue_depth_before,
        algorithm = "drr",
        is_turn_boundary,
    ))]
    pub async fn admit_with_turn_boundary(
        &self,
        flow_id: crate::flow::FlowId,
        work_unit: f64,
        is_turn_boundary: bool,
    ) -> Result<QueueTicket, BackpressureRejected> {
        tracing::Span::current().record("queue_depth_before", self.queue_depth());

        // ── Priority cadence heuristic ──
        let flow = self.registry.get_or_create(flow_id.clone());
        let gap = self.cadence.record_arrival(
            &flow_id,
            std::time::Instant::now(),
            is_turn_boundary,
        );
        self.cadence.classify_and_apply(&flow, &flow_id);
        tracing::Span::current().record("priority", flow.priority());
        tracing::Span::current().record("priority_source", flow.priority_source());

        // ── Priority metrics ──
        self.metrics
            .flow_priority_class
            .with_label_values(&[flow_id.metric_label()])
            .set(flow.priority() as f64);
        self.metrics
            .flow_cadence_state
            .with_label_values(&[flow_id.metric_label()])
            .set(self.cadence.state_of(&flow_id) as u32 as f64);
        if let Some(gap) = gap {
            self.metrics
                .flow_inter_request_seconds
                .with_label_values(&[flow_id.metric_label()])
                .observe(gap.as_secs_f64());
        }

        let enter = std::time::Instant::now();

        // KV policy gate runs FIRST before any flow scheduling.
        self.kv_policy.check().await?;

        // Stall gate: reject new admissions while the backend is stalled.
        // The client gets an immediate 429 + Retry-After and backs off,
        // rather than being admitted, waiting ~30s for the stall watchdog
        // to abort the stream, and getting an Err then.
        if *self.stall_rx.borrow() {
            tracing::info!("admit rejected: backend stalled");
            return Err(BackpressureRejected {
                retry_after: Duration::from_secs(5),
            });
        }

        let result = self.inner.admit(flow_id, work_unit).await;

        let wait_secs = enter.elapsed().as_secs_f64();
        match &result {
            Ok(_) => {
                tracing::info!(
                    decision = "accept",
                    wait_seconds = wait_secs,
                    is_turn_boundary = is_turn_boundary,
                    priority = flow.priority(),
                    "admit decision"
                );
            }
            Err(_) => {
                tracing::info!(
                    decision = "reject",
                    wait_seconds = wait_secs,
                    is_turn_boundary = is_turn_boundary,
                    priority = flow.priority(),
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
        self.inner.queue_depth() + self.kv_policy.delayed_count()
    }

    /// Build a snapshot of the current queue state.
    ///
    /// Waiting count includes both flow-scheduler queue depth and KV-delay
    /// requests so GET /queue reports all pending requests.
    pub fn queue_snapshot(&self) -> crate::flow::QueueSnapshot {
        let inner_snapshot = self.inner.queue_snapshot();
        // Add delayed count to the waiting total.
        let delayed = self.kv_policy.delayed_count();
        crate::flow::QueueSnapshot {
            active: inner_snapshot.active,
            waiting: inner_snapshot.waiting + delayed as u64,
            flows: inner_snapshot.flows,
        }
    }

    /// Return the current credit for the given flow.
    pub fn credit(&self, flow_id: &crate::flow::FlowId) -> i64 {
        self.inner.credit(flow_id)
    }

    /// Report accounting for a completed or cancelled request.
    ///
    /// DRR adjusts per-flow credit based on actual delivered tokens.
    pub fn report_accounting(&self, flow_id: &crate::flow::FlowId, report: AccountingReport) {
        self.inner.report_accounting(flow_id, report)
    }

    /// Return a reference to the flow progress tracker for predictive admit.
    pub fn flow_progress_tracker(&self) -> Arc<flow_progress::FlowProgressTracker> {
        self.flow_progress.clone()
    }

    /// Evict idle flows and cadence entries older than `ttl`.
    ///
    /// Called periodically by the background reaper task (see `main.rs`) to
    /// prevent unbounded growth of the flow and cadence registries from
    /// accumulating session IDs. Returns the number of flows removed from
    /// the flow registry (cadence entries are reaped with the same `ttl`).
    // @lat: [[app#Idle-Flow Reaper]]
    pub fn reap_idle(&self, ttl: Duration) -> usize {
        let removed = self.registry.reap_idle(ttl);
        let cadence_removed = self.cadence.reap_idle(ttl);
        if removed > 0 || cadence_removed > 0 {
            tracing::debug!(
                flows_removed = removed,
                cadence_removed = cadence_removed,
                ttl_secs = ttl.as_secs(),
                "reaped idle flows"
            );
        }
        removed
    }
}
