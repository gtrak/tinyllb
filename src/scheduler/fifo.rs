use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::BackpressureMode;
use crate::flow::{Flow, FlowId, FlowRegistry};
use crate::metrics::Metrics;
use crate::scheduler::backpressure::{fail_fast_retry_after, BackpressureRejected};

/// RAII guard for per-flow queue depth.
///
/// Increments the flow's depth counter by 1 when created and decrements it when
/// dropped. Also sets the Prometheus `llm_queue_depth{flow_id=...}` gauge after
/// each change, so each label reflects that flow's own queued requests.
///
/// On the success path the guard is "consumed" (permit acquired), so the depth
/// is set back to the correct value and the guard is nulled out — its Drop
/// becomes a no-op.  On the cancellation path the guard is simply dropped,
/// correctly releasing the depth increment.
struct DepthGuard {
    flow: Arc<Flow>,
    metrics: Arc<Metrics>,
    flow_label: String,
    active: bool, // false after consume()
}

impl DepthGuard {
    fn new(flow: Arc<Flow>, metrics: Arc<Metrics>, flow_label: String) -> Self {
        let val = flow.depth.fetch_add(1, Ordering::Relaxed) + 1;
        metrics
            .queue_depth
            .with_label_values(&[&flow_label])
            .set(val as f64);
        Self {
            flow,
            metrics,
            flow_label,
            active: true,
        }
    }

    /// Called when the permit is acquired: decrement depth, update gauge,
    /// and nullify the guard so Drop is a no-op.
    fn consume(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let val = self
            .flow
            .depth
            .fetch_sub(1, Ordering::Relaxed)
            .saturating_sub(1);
        self.metrics
            .queue_depth
            .with_label_values(&[&self.flow_label])
            .set(val as f64);
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let val = self
            .flow
            .depth
            .fetch_sub(1, Ordering::Relaxed)
            .saturating_sub(1);
        self.metrics
            .queue_depth
            .with_label_values(&[&self.flow_label])
            .set(val as f64);
    }
}

/// RAII ticket returned by `FifoScheduler::admit`.
///
/// Holds an `OwnedSemaphorePermit` and a reference to the shared metrics.
/// When dropped, it:
/// 1. Releases the semaphore permit (adding it back to the pool).
/// 2. Decrements `llm_active_flows`.
///
/// This guarantees slot release on **all** exit paths: success, error,
/// panic (Drop runs on unwind), and client disconnect (future handler drops).
pub struct QueueTicket {
    /// The flow ID associated with this ticket.
    pub flow_id: FlowId,
    _permit: tokio::sync::OwnedSemaphorePermit,
    metrics: Arc<Metrics>,
}

/// FIFO scheduler with a max-active-flows admission gate.
///
/// Requests call `admit(flow_id)` which may block, reject, or timeout depending on
/// the configured backpressure mode. At most `max_active_flows` requests
/// proceed simultaneously.
///
/// Metrics updated:
/// - `llm_queue_depth{flow_id=...}`: +1 when entering `admit()`, -1 when permit acquired.
/// - `llm_queue_wait_seconds`: observed when the permit is acquired (wall
///   clock from entry to acquire). Instantaneous grants observe ~0.
/// - `llm_active_flows`: +1 inside `QueueTicket` on permit acquire,
///   -1 when the ticket is dropped.
pub struct FifoScheduler {
    /// Semaphore limiting concurrent active flows.
    semaphore: Arc<tokio::sync::Semaphore>,
    /// Shared metrics handle.
    metrics: Arc<Metrics>,
    /// Shared flow registry.
    registry: Arc<FlowRegistry>,
    /// Backpressure mode.
    backpressure_mode: BackpressureMode,
    /// Max queue depth for fail-fast check.
    max_queue_depth: u32,
    /// Max wait duration for hybrid mode.
    max_wait: Duration,
    /// Base duration for Retry-After computation.
    retry_after_base: Duration,
}

impl FifoScheduler {
    /// Create a new FIFO scheduler with the given max active flows,
    /// flow registry, and backpressure configuration.
    pub fn new(
        max_active_flows: u32,
        metrics: Arc<Metrics>,
        registry: Arc<FlowRegistry>,
        backpressure_mode: BackpressureMode,
        max_queue_depth: u32,
        max_wait: Duration,
        retry_after_base: Duration,
    ) -> Self {
        Self {
            semaphore: Arc::new(tokio::sync::Semaphore::new(max_active_flows as usize)),
            metrics,
            registry,
            backpressure_mode,
            max_queue_depth,
            max_wait,
            retry_after_base,
        }
    }

    /// Attempt to admit a request into the active set.
    ///
    /// Behavior depends on the configured backpressure mode:
    /// - **Blocking**: queue indefinitely until a permit is available.
    /// - **FailFast**: if queue depth > max_queue_depth, return `BackpressureRejected`
    ///   immediately. Otherwise, behave like Blocking.
    /// - **Hybrid**: wait up to `max_wait` for a permit. If the wait
    ///   exceeds `max_wait`, return `BackpressureRejected`.
    pub async fn admit(&self, flow_id: FlowId) -> Result<QueueTicket, BackpressureRejected> {
        // Ensure the flow exists in the registry with defaults (atomic entry).
        let flow = self.registry.get_or_create(flow_id.clone());

        let flow_label = flow_id.metric_label().to_string();
        match self.backpressure_mode {
            BackpressureMode::Blocking => self.admit_blocking(flow, flow_label, flow_id).await,
            BackpressureMode::FailFast => self.admit_fail_fast(flow, flow_label, flow_id).await,
            BackpressureMode::Hybrid => self.admit_hybrid(flow, flow_label, flow_id).await,
        }
    }

    /// Blocking mode: identical to pre-issue-06 behavior.
    async fn admit_blocking(
        &self,
        flow: Arc<Flow>,
        flow_label: String,
        flow_id: FlowId,
    ) -> Result<QueueTicket, BackpressureRejected> {
        let enter = Instant::now();

        let mut depth_guard = DepthGuard::new(flow, self.metrics.clone(), flow_label);

        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore should not be closed; we never close it");

        depth_guard.consume();
        record_wait_and_active(self, enter);
        Ok(make_ticket(flow_id, permit, self.metrics.clone()))
    }

    /// Fail-fast mode: reject immediately if the queue is too deep.
    async fn admit_fail_fast(
        &self,
        flow: Arc<Flow>,
        flow_label: String,
        flow_id: FlowId,
    ) -> Result<QueueTicket, BackpressureRejected> {
        let depth = self.queue_depth();
        if depth > self.max_queue_depth {
            let retry_after =
                fail_fast_retry_after(depth, self.max_queue_depth, self.retry_after_base);
            return Err(BackpressureRejected { retry_after });
        }

        // Otherwise proceed with blocking behavior.
        let enter = Instant::now();

        let mut depth_guard = DepthGuard::new(flow, self.metrics.clone(), flow_label);

        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore should not be closed; we never close it");

        depth_guard.consume();
        record_wait_and_active(self, enter);
        Ok(make_ticket(flow_id, permit, self.metrics.clone()))
    }

    /// Hybrid mode: race permit acquisition against a timeout.
    async fn admit_hybrid(
        &self,
        flow: Arc<Flow>,
        flow_label: String,
        flow_id: FlowId,
    ) -> Result<QueueTicket, BackpressureRejected> {
        let enter = Instant::now();

        let mut depth_guard = DepthGuard::new(flow, self.metrics.clone(), flow_label);

        // Use biased select so that if both branches are ready, the acquire
        // branch wins (no spurious rejection). The `acquire_owned` future
        // is polled first.
        let permit = tokio::select!(
            biased;

            // Acquire the semaphore permit.
            permit = self.semaphore.clone().acquire_owned() => {
                permit.expect("semaphore should not be closed; we never close it")
            }

            // Timeout: if we haven't acquired in time, reject.
                _ = tokio::time::sleep(self.max_wait) => {
                    // depth_guard is dropped here, correctly decrementing queue_depth.
                    let depth = self.queue_depth();
                    let retry_after =
                        fail_fast_retry_after(depth, self.max_queue_depth, self.retry_after_base);
                    return Err(BackpressureRejected { retry_after });
                }
        );

        depth_guard.consume();
        record_wait_and_active(self, enter);
        Ok(make_ticket(flow_id, permit, self.metrics.clone()))
    }

    /// Current number of requests inside `admit()` (waiting for a permit).
    ///
    /// Sums per-flow depth counters across all registered flows.
    pub fn queue_depth(&self) -> u32 {
        self.registry.sum_depths()
    }
}

impl Drop for QueueTicket {
    fn drop(&mut self) {
        // The permit is dropped by the `_permit` field, releasing it back
        // to the semaphore. We also decrement the active_flows gauge here.
        self.metrics.active_flows.dec();
    }
}

/// Record the wait time and increment active flows.
fn record_wait_and_active(scheduler: &FifoScheduler, enter: Instant) {
    let wait_secs = enter.elapsed().as_secs_f64();
    scheduler.metrics.queue_wait_seconds.observe(wait_secs);
    scheduler.metrics.active_flows.inc();
}

/// Construct a `QueueTicket` from a flow ID, permit, and metrics handle.
fn make_ticket(
    flow_id: FlowId,
    permit: tokio::sync::OwnedSemaphorePermit,
    metrics: Arc<Metrics>,
) -> QueueTicket {
    QueueTicket {
        flow_id,
        _permit: permit,
        metrics,
    }
}
