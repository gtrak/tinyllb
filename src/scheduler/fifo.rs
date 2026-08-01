use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::BackpressureMode;
use crate::flow::{Flow, FlowId, FlowRegistry, QueueSnapshot};
use crate::metrics::Metrics;
use crate::scheduler::backpressure::{fail_fast_retry_after, BackpressureRejected};
use crate::scheduler::completion_bias::CompletionBiasGate;

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
    waiting_queue: Arc<Mutex<VecDeque<FlowId>>>,
}

impl DepthGuard {
    fn new(
        flow: Arc<Flow>,
        metrics: Arc<Metrics>,
        flow_label: String,
        waiting_queue: Arc<Mutex<VecDeque<FlowId>>>,
        flow_id: FlowId,
    ) -> Self {
        let val = flow.depth.fetch_add(1, Ordering::Relaxed) + 1;
        metrics
            .queue_depth
            .with_label_values(&[&flow_label])
            .set(val as f64);
        waiting_queue.lock().unwrap().push_back(flow_id);
        // Set enqueued_at for starvation detection.
        {
            let mut enq = flow.enqueued_at.write().unwrap();
            *enq = Some(Instant::now());
        }
        Self {
            flow,
            metrics,
            flow_label,
            active: true,
            waiting_queue,
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

        // Clear enqueued_at — the flow is no longer waiting.
        {
            let mut enq = self.flow.enqueued_at.write().unwrap();
            *enq = None;
        }

        // Remove one occurrence of this flow from the waiting queue.
        remove_from_queue(&self.waiting_queue, &self.flow.id);
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

        // Cancellation path: remove one occurrence from the waiting queue.
        remove_from_queue(&self.waiting_queue, &self.flow.id);

        // Clear enqueued_at on cancellation.
        {
            let mut enq = self.flow.enqueued_at.write().unwrap();
            *enq = None;
        }
    }
}

/// Remove one occurrence of `flow_id` from the waiting queue.
fn remove_from_queue(queue: &Mutex<VecDeque<FlowId>>, flow_id: &FlowId) {
    let mut q = queue.lock().unwrap();
    if let Some(pos) = q.iter().position(|id| id == flow_id) {
        q.remove(pos);
    }
}

/// RAII ticket returned by `Scheduler::admit`.
///
/// When dropped, it:
/// 1. Releases the admission slot (semaphore permit for FIFO, internal counter for WFQ).
/// 2. Decrements `llm_active_flows`.
/// 3. Reports service_done for WFQ.
///
/// This guarantees slot release on **all** exit paths: success, error,
/// panic (Drop runs on unwind), and client disconnect (future handler drops).
pub struct QueueTicket {
    /// The flow ID associated with this ticket.
    pub flow_id: FlowId,
    /// Work unit (estimated max_tokens) for this request.
    /// Used by WFQ to track service_done on completion.
    pub work_unit: f64,
    /// Combined drop handler: releases the permit and reports completion.
    /// Wrapped in Option so it can be taken() in Drop (FnOnce can only be
    /// called once, and Drop takes &mut self).
    drop_handler: Option<Box<dyn Send + FnOnce()>>,
}

/// FIFO scheduler with a max-active-flows admission gate.
///
/// Requests call `admit(flow_id, work_unit)` which may block, reject, or timeout depending on
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
    /// FIFO-ordered waiting queue for position reporting.
    /// Pushed on DepthGuard creation, popped on consume or cancellation.
    waiting_queue: Arc<Mutex<VecDeque<FlowId>>>,
    /// Completion bias gate for pre-admit checks.
    completion_bias_gate: Arc<CompletionBiasGate>,
}

impl FifoScheduler {
    /// Create a new FIFO scheduler with the given max active flows,
    /// flow registry, and backpressure configuration.
    ///
    /// Completion bias is disabled by default (starvation_timeout=300s, target=0).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        max_active_flows: u32,
        metrics: Arc<Metrics>,
        registry: Arc<FlowRegistry>,
        backpressure_mode: BackpressureMode,
        max_queue_depth: u32,
        max_wait: Duration,
        retry_after_base: Duration,
    ) -> Self {
        let notify = Arc::new(tokio::sync::Notify::new());
        let gate = Arc::new(CompletionBiasGate::new(
            false, // disabled
            0,
            max_active_flows,
            metrics.clone(),
            registry.clone(),
            notify,
            Duration::from_secs(300),
        ));
        Self {
            semaphore: Arc::new(tokio::sync::Semaphore::new(max_active_flows as usize)),
            metrics,
            registry,
            backpressure_mode,
            max_queue_depth,
            max_wait,
            retry_after_base,
            waiting_queue: Arc::new(Mutex::new(VecDeque::new())),
            completion_bias_gate: gate,
        }
    }

    /// Create a new FIFO scheduler with policy hooks.
    /// Used by `Scheduler::new()` to wire in completion bias and active tracking.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_policies(
        max_active_flows: u32,
        metrics: Arc<Metrics>,
        registry: Arc<FlowRegistry>,
        backpressure_mode: BackpressureMode,
        max_queue_depth: u32,
        max_wait: Duration,
        retry_after_base: Duration,
        policies: super::Policies,
    ) -> Self {
        Self {
            semaphore: Arc::new(tokio::sync::Semaphore::new(max_active_flows as usize)),
            metrics,
            registry,
            backpressure_mode,
            max_queue_depth,
            max_wait,
            retry_after_base,
            waiting_queue: Arc::new(Mutex::new(VecDeque::new())),
            completion_bias_gate: policies.completion_bias,
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
    ///
    /// Completion bias: each sub-method checks the gate AFTER creating the
    /// depth guard so that flows blocked by completion bias are counted in
    /// `queue_depth()`.
    pub async fn admit(
        &self,
        flow_id: FlowId,
        work_unit: f64,
    ) -> Result<QueueTicket, BackpressureRejected> {
        // Ensure the flow exists in the registry with defaults (atomic entry).
        let flow = self.registry.get_or_create(flow_id.clone());
        let flow_label = flow_id.metric_label().to_string();
        match self.backpressure_mode {
            BackpressureMode::Blocking => {
                self.admit_blocking(flow, flow_label, flow_id, work_unit)
                    .await
            }
            BackpressureMode::FailFast => {
                self.admit_fail_fast(flow, flow_label, flow_id, work_unit)
                    .await
            }
            BackpressureMode::Hybrid => {
                self.admit_hybrid(flow, flow_label, flow_id, work_unit)
                    .await
            }
        }
    }

    /// Blocking mode: identical to pre-issue-06 behavior.
    async fn admit_blocking(
        &self,
        flow: Arc<Flow>,
        flow_label: String,
        flow_id: FlowId,
        _work_unit: f64,
    ) -> Result<QueueTicket, BackpressureRejected> {
        let enter = Instant::now();

        let mut depth_guard = DepthGuard::new(
            flow.clone(),
            self.metrics.clone(),
            flow_label.clone(),
            self.waiting_queue.clone(),
            flow_id.clone(),
        );

        // Check completion bias AFTER depth guard creation so that
        // flows blocked at the gate are counted in queue_depth().
        self.completion_bias_gate.check(&flow).await;

        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore should not be closed; we never close it");

        depth_guard.consume();
        record_wait_and_active(self, enter);
        // Track per-flow active status.
        flow.inc_active();

        let metrics = self.metrics.clone();
        let gate = self.completion_bias_gate.clone();
        let flow_clone = flow.clone();
        Ok(make_ticket(flow_id, _work_unit, move || {
            // Permit released by dropping
            drop(permit);
            metrics.active_flows.dec();
            flow_clone.dec_active();
            // Notify completion bias waiters that active count changed.
            gate.notify_waiters();
            // FIFO: no service_done tracking needed
        }))
    }

    /// Fail-fast mode: reject immediately if the queue is too deep.
    async fn admit_fail_fast(
        &self,
        flow: Arc<Flow>,
        flow_label: String,
        flow_id: FlowId,
        _work_unit: f64,
    ) -> Result<QueueTicket, BackpressureRejected> {
        let depth = self.queue_depth();
        if depth > self.max_queue_depth {
            let retry_after =
                fail_fast_retry_after(depth, self.max_queue_depth, self.retry_after_base);
            return Err(BackpressureRejected { retry_after });
        }

        // Otherwise proceed with blocking behavior (depth guard created inside).
        let enter = Instant::now();

        let mut depth_guard = DepthGuard::new(
            flow.clone(),
            self.metrics.clone(),
            flow_label.clone(),
            self.waiting_queue.clone(),
            flow_id.clone(),
        );

        // Check completion bias AFTER depth guard creation so that
        // flows blocked at the gate are counted in queue_depth().
        self.completion_bias_gate.check(&flow).await;

        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore should not be closed; we never close it");

        depth_guard.consume();
        record_wait_and_active(self, enter);
        flow.inc_active();

        let metrics = self.metrics.clone();
        let gate = self.completion_bias_gate.clone();
        let flow_clone = flow.clone();
        Ok(make_ticket(flow_id, _work_unit, move || {
            drop(permit);
            metrics.active_flows.dec();
            flow_clone.dec_active();
            gate.notify_waiters();
        }))
    }

    /// Hybrid mode: race permit acquisition against a timeout.
    async fn admit_hybrid(
        &self,
        flow: Arc<Flow>,
        flow_label: String,
        flow_id: FlowId,
        _work_unit: f64,
    ) -> Result<QueueTicket, BackpressureRejected> {
        let enter = Instant::now();

        let mut depth_guard = DepthGuard::new(
            flow.clone(),
            self.metrics.clone(),
            flow_label.clone(),
            self.waiting_queue.clone(),
            flow_id.clone(),
        );

        // Check completion bias with a timeout equal to max_wait.
        // If the gate blocks too long, the flow proceeds to the backpressure
        // handler which may reject it via its own timeout.
        tokio::time::timeout(self.max_wait, self.completion_bias_gate.check(&flow))
            .await
            .ok();

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
                    // depth_guard is dropped here, correctly decrementing queue_depth
                    // and removing from waiting queue.
                    let depth = self.queue_depth();
                    let retry_after =
                        fail_fast_retry_after(depth, self.max_queue_depth, self.retry_after_base);
                    return Err(BackpressureRejected { retry_after });
                }
        );

        depth_guard.consume();
        record_wait_and_active(self, enter);
        flow.inc_active();

        let metrics = self.metrics.clone();
        let gate = self.completion_bias_gate.clone();
        let flow_clone = flow.clone();
        Ok(make_ticket(flow_id, _work_unit, move || {
            drop(permit);
            metrics.active_flows.dec();
            flow_clone.dec_active();
            gate.notify_waiters();
        }))
    }

    /// Current number of requests inside `admit()` (waiting for a permit).
    ///
    /// Sums per-flow depth counters across all registered flows.
    pub fn queue_depth(&self) -> u32 {
        self.registry.sum_depths()
    }

    /// Build a snapshot of the current queue state.
    ///
    /// Returns the number of active flows, the total waiting count, and a
    /// list of per-flow positions (1-indexed) for flows currently waiting.
    pub fn queue_snapshot(&self) -> QueueSnapshot {
        let active = self.metrics.active_flows.get() as u64;
        let waiting = self.queue_depth() as u64;

        let queue = self.waiting_queue.lock().unwrap();
        // Drain the lock scope.
        let wait_ids: Vec<FlowId> = queue.iter().cloned().collect();

        self.registry.queue_snapshot(active, waiting, wait_ids)
    }
}

impl QueueTicket {
    /// Disarm this ticket so its drop handler does NOT run on Drop.
    ///
    /// Used by the WFQ admission loop when the oneshot send fails: the receiver
    /// is gone (timeout or abort), so we must prevent the drop handler from
    /// decrementing `active_flows`, crediting `service_done`, and releasing the
    /// permit. The caller is responsible for releasing the permit exactly once.
    pub fn disarm(&mut self) {
        self.drop_handler.take();
    }
}

impl Drop for QueueTicket {
    fn drop(&mut self) {
        // Take the handler out of the Option (FnOnce can only be called once).
        if let Some(handler) = self.drop_handler.take() {
            handler();
        }
    }
}

/// Record the wait time and increment active flows.
fn record_wait_and_active(scheduler: &FifoScheduler, enter: Instant) {
    let wait_secs = enter.elapsed().as_secs_f64();
    scheduler.metrics.queue_wait_seconds.observe(wait_secs);
    scheduler.metrics.active_flows.inc();
}

/// Construct a `QueueTicket` from a flow ID, work unit, and a drop handler closure.
///
/// The `drop_handler` closure is called on drop to release the permit
/// and report completion. For FIFO this releases the semaphore permit;
/// for WFQ it decrements the internal permit counter and increments service_done.
pub fn make_ticket(
    flow_id: FlowId,
    work_unit: f64,
    drop_handler: impl FnOnce() + Send + 'static,
) -> QueueTicket {
    QueueTicket {
        flow_id,
        work_unit,
        drop_handler: Some(Box::new(drop_handler)),
    }
}
