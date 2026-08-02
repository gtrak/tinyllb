//! Weighted Fair Queueing (WFQ) scheduler.
//!
//! When a slot frees, the next request is chosen from the flow with the
//! minimum `service_done / weight` ratio (ties broken FIFO by enqueue time).
//! This implements PRD §6.4 V1 — token allocation ∝ weight.
//!
//! Architecture:
//! - `admit()` queues the request and waits on a per-request oneshot channel.
//! - An internal admission loop selects flows when permits are available,
//!   wakes the head waiter via the oneshot, and returns a ticket.
//! - When the ticket drops, it:
//!   1. Decrements active_flows.
//!   2. Increments service_done for the flow.
//!   3. Releases the internal permit.
//!   4. Notifies the admission loop to try selecting again.
//!
//! Priority & starvation: `try_select` checks for starved flows first
//! (force-select), then picks the highest-priority eligible flow,
//! using the base WFQ ratio as a tiebreak.

use std::collections::hash_map::Entry;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::BackpressureMode;
use crate::flow::{Flow, FlowId, FlowRegistry, QueueSnapshot};
use crate::metrics::Metrics;
use crate::scheduler::backpressure::{fail_fast_retry_after, BackpressureRejected};
use crate::scheduler::completion_bias::CompletionBiasGate;
use crate::scheduler::fifo::make_ticket;
use crate::scheduler::fifo::QueueTicket;
use crate::scheduler::priority;
use crate::scheduler::starvation;

/// Per-request entry in a flow's waiting queue.
struct Pending {
    /// Unique identifier for this pending entry, used for precise removal
    /// on cancellation (prevents sibling-kill bug).
    pending_id: u64,
    /// Sender half of the oneshot channel. The admission loop sends the ticket
    /// through this when the request is selected.
    tx: tokio::sync::oneshot::Sender<QueueTicket>,
    /// When this request was enqueued (for FIFO tie-breaking).
    enqueued_at: Instant,
    /// Work unit (max_tokens estimate) for this request.
    work_unit: f64,
}

/// Shared mutable state for the WFQ scheduler.
struct WfqState {
    /// Per-flow waiting queues. Each queue holds pending requests in FIFO order.
    waiting: std::collections::HashMap<FlowId, VecDeque<Pending>>,
    /// Per-flow service_done counters (stored as f64 bits in AtomicU64).
    /// Sum of work_units of completed requests for each flow.
    service_done: std::collections::HashMap<FlowId, AtomicU64>,
    /// Number of available permits (max_active_flows - currently active).
    available_permits: u32,
    /// Monotonically increasing counter for unique pending IDs.
    next_pending_id: u64,
}

/// Shared state for the WFQ scheduler.
struct SharedState {
    inner: Mutex<WfqState>,
    /// Notify the admission loop that something changed (slot freed, new request).
    notify: Arc<tokio::sync::Notify>,
}

/// WFQ scheduler with per-flow weighted fair queuing.
pub struct WfqScheduler {
    /// Shared state, cloned as Arc for the admission loop and ticket closures.
    state: Arc<SharedState>,
    /// Shared metrics handle.
    metrics: Arc<Metrics>,
    /// Shared flow registry.
    registry: Arc<FlowRegistry>,
    /// Backpressure mode.
    backpressure_mode: BackpressureMode,
    /// Max queue depth for fail-fast check.
    max_queue_depth: u32,
    /// Max wait duration for hybrid mode.
    max_wait: std::time::Duration,
    /// Base duration for Retry-After computation.
    retry_after_base: std::time::Duration,
    /// FIFO-ordered waiting queue for GET /queue reporting.
    waiting_queue: Arc<Mutex<VecDeque<FlowId>>>,
    /// Completion bias gate for pre-admit checks.
    completion_bias_gate: Arc<CompletionBiasGate>,
}

impl WfqScheduler {
    /// Create a new WFQ scheduler.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        max_active_flows: u32,
        metrics: Arc<Metrics>,
        registry: Arc<FlowRegistry>,
        backpressure_mode: BackpressureMode,
        max_queue_depth: u32,
        max_wait: std::time::Duration,
        retry_after_base: std::time::Duration,
    ) -> Self {
        let notify = Arc::new(tokio::sync::Notify::new());
        let flow_progress = Arc::new(super::flow_progress::FlowProgressTracker::new());
        let gate = Arc::new(CompletionBiasGate::new(
            false,
            0,
            false, // predictive_admit
            max_active_flows,
            metrics.clone(),
            registry.clone(),
            notify,
            Duration::from_secs(300),
            flow_progress,
        ));
        Self::new_inner(
            max_active_flows,
            metrics,
            registry,
            backpressure_mode,
            max_queue_depth,
            max_wait,
            retry_after_base,
            Duration::from_secs(300),
            gate,
        )
    }

    /// Create a new WFQ scheduler with policy hooks.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_policies(
        max_active_flows: u32,
        metrics: Arc<Metrics>,
        registry: Arc<FlowRegistry>,
        backpressure_mode: BackpressureMode,
        max_queue_depth: u32,
        max_wait: std::time::Duration,
        retry_after_base: std::time::Duration,
        starvation_timeout: Duration,
        policies: super::Policies,
    ) -> Self {
        Self::new_inner(
            max_active_flows,
            metrics,
            registry,
            backpressure_mode,
            max_queue_depth,
            max_wait,
            retry_after_base,
            starvation_timeout,
            policies.completion_bias,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_inner(
        max_active_flows: u32,
        metrics: Arc<Metrics>,
        registry: Arc<FlowRegistry>,
        backpressure_mode: BackpressureMode,
        max_queue_depth: u32,
        max_wait: std::time::Duration,
        retry_after_base: std::time::Duration,
        starvation_timeout: Duration,
        completion_bias_gate: Arc<CompletionBiasGate>,
    ) -> Self {
        let notify = Arc::new(tokio::sync::Notify::new());
        let state = Arc::new(SharedState {
            inner: Mutex::new(WfqState {
                waiting: std::collections::HashMap::new(),
                service_done: std::collections::HashMap::new(),
                available_permits: max_active_flows,
                next_pending_id: 0,
            }),
            notify: notify.clone(),
        });

        // Spawn the admission loop.
        let state_clone = state.clone();
        let metrics_clone = metrics.clone();
        let registry_clone = registry.clone();
        let gate_clone = completion_bias_gate.clone();
        tokio::spawn(Self::admission_loop(
            state_clone,
            metrics_clone,
            registry_clone,
            starvation_timeout,
            gate_clone,
        ));

        Self {
            state,
            metrics,
            registry,
            backpressure_mode,
            max_queue_depth,
            max_wait,
            retry_after_base,
            waiting_queue: Arc::new(Mutex::new(VecDeque::new())),
            completion_bias_gate,
        }
    }

    /// Background admission loop: when notified (slot freed or new request),
    /// try to select the best flow and wake its head waiter.
    async fn admission_loop(
        state: Arc<SharedState>,
        metrics: Arc<Metrics>,
        registry: Arc<FlowRegistry>,
        starvation_timeout: Duration,
        gate: Arc<CompletionBiasGate>,
    ) {
        loop {
            state.notify.notified().await;

            // Keep trying to select while permits are available and flows are waiting.
            loop {
                let selection = Self::try_select(&state, &registry, &metrics, starvation_timeout);
                match selection {
                    None => break,
                    Some((flow_id, pending, work_unit)) => {
                        // Build the ticket with a drop closure that:
                        // 1. Decrements active_flows
                        // 2. Increments service_done for this flow
                        // 3. Releases the permit
                        // 4. Notifies the admission loop
                        let metrics_clone = metrics.clone();
                        let flow_id_for_ticket = flow_id.clone();
                        let state_clone = state.clone();
                        let gate_clone = gate.clone();
                        let flow_for_active = registry.get_or_create(flow_id.clone());

                        let ticket =
                            make_ticket(flow_id_for_ticket.clone(), work_unit, move || {
                                metrics_clone.active_flows.dec();
                                flow_for_active.dec_active();

                                // Increment service_done for this flow.
                                {
                                    let mut s = state_clone.inner.lock().unwrap();
                                    let bits = f64::to_bits(work_unit);
                                    match s.service_done.entry(flow_id_for_ticket.clone()) {
                                        Entry::Occupied(entry) => {
                                            let current =
                                                f64::from_bits(entry.get().load(Ordering::Relaxed));
                                            entry.get().store(
                                                f64::to_bits(current + work_unit),
                                                Ordering::Relaxed,
                                            );
                                        }
                                        Entry::Vacant(entry) => {
                                            entry.insert(AtomicU64::new(bits));
                                        }
                                    }
                                    // Release the permit.
                                    s.available_permits += 1;
                                }

                                // Notify the admission loop.
                                state_clone.notify.notify_one();
                                // Notify completion bias waiters.
                                gate_clone.notify_waiters();
                            });

                        // Send the ticket to the waiting request.
                        match pending.tx.send(ticket) {
                            Ok(()) => {}
                            Err(mut ticket) => {
                                // Request was cancelled or timed out.  Disarm the ticket so its
                                // drop handler (which would decrement active_flows, credit
                                // service_done, and release the permit) does NOT run.  We only
                                // need to release the permit once here.
                                ticket.disarm();
                                let mut s = state.inner.lock().unwrap();
                                s.available_permits += 1;
                                state.notify.notify_one();
                            }
                        }
                    }
                }
            }
        }
    }

    /// Try to select the next flow from the waiting queue.
    ///
    /// Selection order:
    /// 1. Starved flows are force-selected first (bypassing normal rules).
    /// 2. Among remaining eligible flows, highest priority wins.
    /// 3. Ties broken by min service_done/weight (base WFQ rule).
    /// 4. Further ties broken by FIFO (earliest enqueue time).
    fn try_select(
        state: &Arc<SharedState>,
        registry: &FlowRegistry,
        metrics: &Metrics,
        starvation_timeout: Duration,
    ) -> Option<(FlowId, Pending, f64)> {
        let mut s = state.inner.lock().unwrap();
        if s.available_permits == 0 {
            return None;
        }

        // Phase 1: Check for starved flows (force-select if found).
        // Collect candidate flow IDs first to avoid borrow conflicts.
        let starved_candidates: Vec<FlowId> = s
            .waiting
            .iter()
            .filter(|(_, q)| !q.is_empty())
            .map(|(fid, _)| fid.clone())
            .collect();

        for flow_id in &starved_candidates {
            let flow = registry.get_or_create(flow_id.clone());
            if let Some(wait) = starvation::is_starved(&flow, starvation_timeout) {
                // Force-select this starved flow.
                starvation::record_force_admit(metrics, &flow, wait);
                let pending = s.waiting.get_mut(flow_id).and_then(|q| q.pop_front())?;
                let work_unit = pending.work_unit;
                s.available_permits -= 1;
                // Clear enqueued_at — the flow is now being served.
                {
                    let mut enq = flow.enqueued_at.write().unwrap();
                    *enq = None;
                }
                drop(s);
                return Some((flow_id.clone(), pending, work_unit));
            }
        }
        drop(starved_candidates);

        // Phase 2: Build candidates with priority and base WFQ score.
        let mut candidates: Vec<priority::FlowCandidate> = Vec::new();

        for (flow_id, queue) in s.waiting.iter() {
            if queue.is_empty() {
                continue;
            }

            let flow = registry.get_or_create(flow_id.clone());
            let weight = flow.weight();
            if weight <= 0.0 {
                continue;
            }

            let service_bits = match s.service_done.get(flow_id) {
                Some(counter) => counter.load(Ordering::Relaxed),
                None => 0,
            };
            let service_done = f64::from_bits(service_bits);
            let ratio = service_done / weight;

            let head = queue.front().unwrap();

            candidates.push(priority::FlowCandidate {
                flow_id: flow_id.clone(),
                priority: flow.priority(),
                enqueued_at: head.enqueued_at,
                base_score: ratio,
            });
        }

        // Select best candidate using priority-aware selection.
        let flow_id = priority::select_best(&candidates)?;

        let pending = s.waiting.get_mut(&flow_id).and_then(|q| q.pop_front())?;

        // Extract work_unit before moving pending.
        let work_unit = pending.work_unit;

        // Decrement available permits.
        s.available_permits -= 1;

        // Clear enqueued_at — the flow is now being served.
        let flow = registry.get_or_create(flow_id.clone());
        {
            let mut enq = flow.enqueued_at.write().unwrap();
            *enq = None;
        }

        drop(s);

        Some((flow_id, pending, work_unit))
    }

    /// Attempt to admit a request into the active set.
    pub async fn admit(
        &self,
        flow_id: FlowId,
        work_unit: f64,
    ) -> Result<QueueTicket, BackpressureRejected> {
        // Ensure the flow exists in the registry with defaults.
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

    async fn admit_blocking(
        &self,
        flow: Arc<Flow>,
        flow_label: String,
        flow_id: FlowId,
        work_unit: f64,
    ) -> Result<QueueTicket, BackpressureRejected> {
        // RAII guard ensures depth/waiting_queue cleanup on any cancellation
        // path (task abort, client disconnect, etc.).
        let mut guard = WfqAdmitGuard::new(
            flow.clone(),
            self.metrics.clone(),
            flow_label.clone(),
            flow_id.clone(),
            self.waiting_queue.clone(),
            self.state.clone(),
        );

        // Check completion bias AFTER guard creation (counts in queue_depth).
        // Blocking mode: gate can wait indefinitely (starvation timeout handles it).
        self.completion_bias_gate.check(&flow).await;

        // Create the oneshot channel.
        let (tx, rx) = tokio::sync::oneshot::channel();

        // Push the pending entry into the waiting queue and assign a unique ID.
        let enter = Instant::now();
        let pending_id = {
            let mut state = self.state.inner.lock().unwrap();
            let my_id = state.next_pending_id;
            state.next_pending_id += 1;
            state
                .waiting
                .entry(flow_id.clone())
                .or_default()
                .push_back(Pending {
                    pending_id: my_id,
                    tx,
                    enqueued_at: enter,
                    work_unit,
                });
            my_id
        };
        guard.set_pending_id(pending_id);

        // Set enqueued_at for starvation detection.
        {
            let mut enq = flow.enqueued_at.write().unwrap();
            *enq = Some(enter);
        }

        // Notify the admission loop.
        self.state.notify.notify_one();

        // Wait for the admission loop to select us.
        let ticket = rx.await.map_err(|_| {
            // Channel closed — shouldn't happen in blocking mode.
            // Guard will clean up depth and waiting_queue on drop.
            BackpressureRejected {
                retry_after: std::time::Duration::from_secs(1),
            }
        })?;

        // Consume the guard — we're now active, no further cleanup needed.
        guard.consume();

        // Record wait time and active flows.
        let wait_secs = enter.elapsed().as_secs_f64();
        self.metrics.queue_wait_seconds.observe(wait_secs);
        self.metrics.active_flows.inc();
        // Track per-flow active status.
        flow.inc_active();

        Ok(ticket)
    }

    async fn admit_fail_fast(
        &self,
        flow: Arc<Flow>,
        flow_label: String,
        flow_id: FlowId,
        work_unit: f64,
    ) -> Result<QueueTicket, BackpressureRejected> {
        // Check depth BEFORE incrementing (matching Fifo behavior).
        let depth = self.queue_depth();
        if depth > self.max_queue_depth {
            let retry_after =
                fail_fast_retry_after(depth, self.max_queue_depth, self.retry_after_base);
            return Err(BackpressureRejected { retry_after });
        }

        // Proceed with blocking behavior (depth managed inside).
        self.admit_blocking(flow, flow_label, flow_id, work_unit)
            .await
    }

    async fn admit_hybrid(
        &self,
        flow: Arc<Flow>,
        flow_label: String,
        flow_id: FlowId,
        work_unit: f64,
    ) -> Result<QueueTicket, BackpressureRejected> {
        // RAII guard ensures depth/waiting_queue cleanup on any cancellation
        // path (task abort, timeout, etc.).
        let mut guard = WfqAdmitGuard::new(
            flow.clone(),
            self.metrics.clone(),
            flow_label.clone(),
            flow_id.clone(),
            self.waiting_queue.clone(),
            self.state.clone(),
        );

        // Check completion bias with a timeout equal to max_wait.
        // If the gate blocks too long, the flow proceeds to the backpressure
        // handler which may reject it via its own timeout.
        tokio::time::timeout(self.max_wait, self.completion_bias_gate.check(&flow))
            .await
            .ok();

        // Create the oneshot channel.
        let (tx, rx) = tokio::sync::oneshot::channel();

        // Push the pending entry and assign a unique ID.
        let enter = Instant::now();
        let pending_id = {
            let mut state = self.state.inner.lock().unwrap();
            let my_id = state.next_pending_id;
            state.next_pending_id += 1;
            state
                .waiting
                .entry(flow_id.clone())
                .or_default()
                .push_back(Pending {
                    pending_id: my_id,
                    tx,
                    enqueued_at: enter,
                    work_unit,
                });
            my_id
        };
        guard.set_pending_id(pending_id);

        // Set enqueued_at for starvation detection.
        {
            let mut enq = flow.enqueued_at.write().unwrap();
            *enq = Some(enter);
        }

        self.state.notify.notify_one();

        // Race: wait for ticket OR timeout.
        let result = tokio::select!(
            biased;
            ticket = rx => {
                match ticket {
                    Ok(t) => {
                        // Consume the guard — we're now active.
                        guard.consume();
                        let wait_secs = enter.elapsed().as_secs_f64();
                        self.metrics.queue_wait_seconds.observe(wait_secs);
                        self.metrics.active_flows.inc();
                        flow.inc_active();
                        Ok(t)
                    }
                    Err(_) => {
                        // Guard will clean up on drop.
                        Err(BackpressureRejected {
                            retry_after: std::time::Duration::from_secs(1),
                        })
                    }
                }
            }
            _ = tokio::time::sleep(self.max_wait) => {
                // Timeout: guard cleans up depth/waiting_queue on drop.
                // The tx is dropped here, closing the channel. Admission loop will skip.
                let depth = self.queue_depth();
                let retry_after =
                    fail_fast_retry_after(depth, self.max_queue_depth, self.retry_after_base);
                Err(BackpressureRejected { retry_after })
            }
        );

        result
    }

    /// Current number of requests waiting in the queue.
    pub fn queue_depth(&self) -> u32 {
        self.registry.sum_depths()
    }

    /// Build a snapshot of the current queue state.
    pub fn queue_snapshot(&self) -> QueueSnapshot {
        let active = self.metrics.active_flows.get() as u64;
        let waiting = self.queue_depth() as u64;

        let queue = self.waiting_queue.lock().unwrap();
        let wait_ids: Vec<FlowId> = queue.iter().cloned().collect();

        self.registry.queue_snapshot(active, waiting, wait_ids)
    }

    /// Return the total service_done for the given flow.
    /// Used by tests to verify weight-ratio adherence.
    pub fn service_done(&self, flow_id: &FlowId) -> f64 {
        let s = self.state.inner.lock().unwrap();
        match s.service_done.get(flow_id) {
            Some(counter) => f64::from_bits(counter.load(Ordering::Relaxed)),
            None => 0.0,
        }
    }
}

/// RAII guard for WFQ blocking-mode admission.
struct WfqAdmitGuard {
    flow: Arc<Flow>,
    metrics: Arc<Metrics>,
    flow_label: String,
    flow_id: FlowId,
    waiting_queue: Arc<Mutex<VecDeque<FlowId>>>,
    state: Arc<SharedState>,
    active: bool,
    /// The unique ID of this guard's Pending entry.
    pending_id: Option<u64>,
}

impl WfqAdmitGuard {
    fn new(
        flow: Arc<Flow>,
        metrics: Arc<Metrics>,
        flow_label: String,
        flow_id: FlowId,
        waiting_queue: Arc<Mutex<VecDeque<FlowId>>>,
        state: Arc<SharedState>,
    ) -> Self {
        let val = flow.depth.fetch_add(1, Ordering::Relaxed) + 1;
        metrics
            .queue_depth
            .with_label_values(&[&flow_label])
            .set(val as f64);
        waiting_queue.lock().unwrap().push_back(flow_id.clone());
        Self {
            flow,
            metrics,
            flow_label,
            flow_id,
            waiting_queue,
            state,
            active: true,
            pending_id: None,
        }
    }

    fn set_pending_id(&mut self, id: u64) {
        self.pending_id = Some(id);
    }

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
        remove_from_queue(&self.waiting_queue, &self.flow_id);
    }
}

impl Drop for WfqAdmitGuard {
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
        remove_from_queue(&self.waiting_queue, &self.flow_id);
        let mut s = self.state.inner.lock().unwrap();
        if let Some(queue) = s.waiting.get_mut(&self.flow_id) {
            if let Some(my_id) = self.pending_id {
                queue.retain(|p| p.pending_id != my_id);
            }
            if queue.is_empty() {
                s.waiting.remove(&self.flow_id);
            }
        }
        // Clear enqueued_at on cancellation.
        {
            let mut enq = self.flow.enqueued_at.write().unwrap();
            *enq = None;
        }
    }
}

fn remove_from_queue(queue: &Mutex<VecDeque<FlowId>>, flow_id: &FlowId) {
    let mut q = queue.lock().unwrap();
    if let Some(pos) = q.iter().position(|id| id == flow_id) {
        q.remove(pos);
    }
}
