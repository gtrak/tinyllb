//! Deficit Round Robin (DRR) scheduler.
//!
//! Each admission opportunity, waiting flows accumulate `credit += weight`.
//! The flow at the head of the round-robin cursor whose `credit >= cost` is
//! selected; it is served and `credit -= cost`.  If no flow has enough credit,
//! no admission this tick (wait for the next completion event).
//!
//! Credit is per-flow, stored as `i64` in `Flow.credit: AtomicI64`.
//! Weight (f64) is rounded to `i64` for accumulation; work_unit (f64) is
//! rounded to `i64` for consumption.  When a flow's queue empties, its
//! credit is reset to 0 (avoid unbounded growth).
//!
//! Architecture mirrors WFQ:
//! - Same `Pending`, `SharedState`, admission loop, RAII guard, backpressure,
//!   queue_snapshot, ticket/disarm pattern.
//! - Selection differs: DRR round-robin cursor with credit bookkeeping.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::config::BackpressureMode;
use crate::flow::{Flow, FlowId, FlowRegistry, QueueSnapshot};
use crate::metrics::Metrics;
use crate::scheduler::backpressure::{fail_fast_retry_after, BackpressureRejected};
use crate::scheduler::fifo::{make_ticket, QueueTicket};

/// Per-request entry in a flow's waiting queue.
struct Pending {
    /// Unique identifier for this pending entry, used for precise removal
    /// on cancellation (prevents sibling-kill bug).
    pending_id: u64,
    /// Sender half of the oneshot channel. The admission loop sends the ticket
    /// through this when the request is selected.
    tx: tokio::sync::oneshot::Sender<QueueTicket>,
    /// When this request was enqueued (kept for architectural symmetry with WFQ).
    _enqueued_at: Instant,
    /// Work unit (max_tokens estimate) for this request.
    work_unit: f64,
}

/// Shared mutable state for the DRR scheduler.
struct DrrState {
    /// Per-flow waiting queues. Each queue holds pending requests in FIFO order.
    waiting: std::collections::HashMap<FlowId, VecDeque<Pending>>,
    /// Round-robin cursor: ordered list of flow IDs that have waiting requests.
    /// Flows are removed when their queue empties (credit reset happens in the
    /// guard on empty-queue detection).
    rr_cursor: VecDeque<FlowId>,
    /// Number of available permits (max_active_flows - currently active).
    available_permits: u32,
    /// Monotonically increasing counter for unique pending IDs.
    next_pending_id: u64,
}

/// Shared state for the DRR scheduler.
struct SharedState {
    inner: Mutex<DrrState>,
    /// Notify the admission loop that something changed (slot freed, new request).
    notify: Arc<tokio::sync::Notify>,
}

/// DRR scheduler with per-flow deficit round-robin credit bookkeeping.
pub struct DrrScheduler {
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
}

impl DrrScheduler {
    /// Create a new DRR scheduler.
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
        let state = Arc::new(SharedState {
            inner: Mutex::new(DrrState {
                waiting: std::collections::HashMap::new(),
                rr_cursor: VecDeque::new(),
                available_permits: max_active_flows,
                next_pending_id: 0,
            }),
            notify: Arc::new(tokio::sync::Notify::new()),
        });

        // Spawn the admission loop.
        let state_clone = state.clone();
        let metrics_clone = metrics.clone();
        let registry_clone = registry.clone();
        tokio::spawn(Self::admission_loop(
            state_clone,
            metrics_clone,
            registry_clone,
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
        }
    }

    /// Background admission loop: when notified (slot freed or new request),
    /// try to select the best flow using DRR round-robin with credit,
    /// and wake its head waiter.
    async fn admission_loop(
        state: Arc<SharedState>,
        metrics: Arc<Metrics>,
        registry: Arc<FlowRegistry>,
    ) {
        loop {
            state.notify.notified().await;

            // Keep trying to select while permits are available and flows are waiting.
            // Each call to try_select accumulates credit for ALL waiting flows,
            // so even if nothing is eligible yet, credit is progressing.
            // We keep looping until no permits, no waiting flows, or no credit
            // was accumulated (meaning all remaining flows have weight <= 0 and
            // can never become eligible — classic DRR active-list semantics).
            loop {
                // Check preconditions before each round.
                let (has_permits, has_waiting) = {
                    let s = state.inner.lock().unwrap();
                    (s.available_permits > 0, !s.rr_cursor.is_empty())
                };
                if !has_permits || !has_waiting {
                    break;
                }

                let (selection, credit_accumulated) = Self::try_select(&state, &registry, &metrics);
                match selection {
                    None => {
                        if credit_accumulated {
                            // No flow eligible this round, but credit was accumulated.
                            // Continue the inner loop to accumulate more credit.
                        } else {
                            // No progress possible: all flows have non-positive weight
                            // (or no waiting flows with positive weight remain).
                            // Classic DRR active-list semantics — break to avoid
                            // spinning forever on zero-weight flows.
                            break;
                        }
                    }
                    Some((flow_id, pending, work_unit)) => {
                        // Build the ticket with a drop closure that:
                        // 1. Decrements active_flows
                        // 2. Releases the permit
                        // 3. Notifies the admission loop
                        let metrics_clone = metrics.clone();
                        let flow_id_for_ticket = flow_id.clone();
                        let state_clone = state.clone();

                        let ticket =
                            make_ticket(flow_id_for_ticket.clone(), work_unit, move || {
                                metrics_clone.active_flows.dec();

                                // Release the permit.
                                {
                                    let mut s = state_clone.inner.lock().unwrap();
                                    s.available_permits += 1;
                                }

                                // notify_one stores a permit when no waiter is registered,
                                // preventing lost-wakeup between inner-drain break and notified().await.
                                state_clone.notify.notify_one();
                            });

                        // Send the ticket to the waiting request.
                        match pending.tx.send(ticket) {
                            Ok(()) => {}
                            Err(mut ticket) => {
                                // Request was cancelled or timed out.  Disarm the ticket so its
                                // drop handler does NOT run.  We only need to release the permit once.
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

    /// Try to select the next flow using DRR round-robin with credit.
    ///
    /// Algorithm per full round:
    /// 1. For EVERY flow in the cursor, accumulate credit: `credit += floor(weight)`.
    /// 2. Find the first flow (in RR order) whose `credit >= cost`.
    /// 3. If found: serve it, deduct cost from credit, return selection.
    /// 4. If not found: rotate cursor, return None (no admission this tick).
    ///
    /// Returns `(Option<(flow_id, pending, work_unit)>, credit_accumulated)`.
    /// `credit_accumulated` is true if ANY flow accumulated credit this round.
    /// When false, the caller should break the inner drain loop (no progress
    /// is possible — all remaining flows have non-positive weight).
    ///
    /// When a flow's queue empties (detected in the guard), credit is reset to 0.
    fn try_select(
        state: &Arc<SharedState>,
        registry: &FlowRegistry,
        metrics: &Metrics,
    ) -> (Option<(FlowId, Pending, f64)>, bool) {
        let mut s = state.inner.lock().unwrap();
        if s.available_permits == 0 {
            return (None, false);
        }

        if s.rr_cursor.is_empty() {
            return (None, false);
        }

        // Phase 1: Accumulate credit for ALL waiting flows in this round.
        // This ensures every waiting flow gets its weight-based credit increment.
        let mut eligible_flows: Vec<(usize, FlowId)> = Vec::new();
        let mut credit_accumulated = false;

        for (idx, flow_id) in s.rr_cursor.iter().enumerate() {
            // Check if this flow's queue is empty.
            let queue = match s.waiting.get(flow_id) {
                Some(q) => q,
                None => {
                    // Queue was removed — skip (cursor will be cleaned up later).
                    continue;
                }
            };

            if queue.is_empty() {
                // Empty queue — will be cleaned up later.
                continue;
            }

            let flow = registry.get_or_create(flow_id.clone());
            let weight_i64 = flow.weight() as i64;
            if weight_i64 <= 0 {
                continue; // Skip zero/negative weight flows.
            }

            let current_credit = flow.credit.load(Ordering::Relaxed);
            let accumulated = current_credit + weight_i64;
            flow.credit.store(accumulated, Ordering::Relaxed);
            metrics
                .flow_credit
                .with_label_values(&[flow_id.metric_label()])
                .set(accumulated as f64);
            credit_accumulated = true;

            // Check if this flow is now eligible.
            let head = queue.front().expect("queue should not be empty");
            let cost_i64 = head.work_unit as i64;

            if accumulated >= cost_i64 {
                eligible_flows.push((idx, flow_id.clone()));
            }
        }

        // Phase 2: Select the first eligible flow in RR order.
        if let Some((_idx, selected_flow_id)) = eligible_flows.first().cloned() {
            let selected_flow_id_clone = selected_flow_id.clone();

            // Pop the head pending from this flow's queue.
            let pending = s
                .waiting
                .get_mut(&selected_flow_id_clone)
                .and_then(|q| q.pop_front())
                .expect("eligible flow should have pending entry");
            let work_unit = pending.work_unit;

            // Deduct cost from credit.
            let flow = registry.get_or_create(selected_flow_id_clone.clone());
            let new_credit = flow.credit.load(Ordering::Relaxed) - (work_unit as i64);
            flow.credit.store(new_credit, Ordering::Relaxed);
            metrics
                .flow_credit
                .with_label_values(&[selected_flow_id_clone.metric_label()])
                .set(new_credit as f64);

            // If this flow's queue is now empty, remove from cursor and reset credit.
            if s.waiting
                .get(&selected_flow_id_clone)
                .is_none_or(|q| q.is_empty())
            {
                s.waiting.remove(&selected_flow_id_clone);
                flow.credit.store(0, Ordering::Relaxed);
                metrics
                    .flow_credit
                    .with_label_values(&[selected_flow_id_clone.metric_label()])
                    .set(0.0);
            }

            // Rotate: move served flow to the back of cursor (if still has requests).
            // Remove from its current position and push back if it still has work.
            s.rr_cursor.retain(|id| id != &selected_flow_id_clone);
            if s.waiting
                .get(&selected_flow_id_clone)
                .is_some_and(|q| !q.is_empty())
            {
                s.rr_cursor.push_back(selected_flow_id_clone.clone());
            }

            s.available_permits -= 1;
            drop(s);

            return (Some((selected_flow_id_clone, pending, work_unit)), true);
        }

        // No flow eligible this round. Rotate the cursor to maintain fairness.
        if let Some(front) = s.rr_cursor.pop_front() {
            s.rr_cursor.push_back(front);
        }

        // Clean up empty queues from cursor and waiting map.
        let empty_flows: Vec<FlowId> = s
            .waiting
            .iter()
            .filter(|(_, q)| q.is_empty())
            .map(|(fid, _)| fid.clone())
            .collect();
        for fid in &empty_flows {
            s.waiting.remove(fid);
            s.rr_cursor.retain(|id| id != fid);
        }

        (None, credit_accumulated)
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
        let enter = Instant::now();

        match self.backpressure_mode {
            BackpressureMode::Blocking => {
                self.admit_blocking(flow, flow_label, flow_id, work_unit, enter)
                    .await
            }
            BackpressureMode::FailFast => {
                self.admit_fail_fast(flow, flow_label, flow_id, work_unit, enter)
                    .await
            }
            BackpressureMode::Hybrid => {
                self.admit_hybrid(flow, flow_label, flow_id, work_unit, enter)
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
        enter: Instant,
    ) -> Result<QueueTicket, BackpressureRejected> {
        // RAII guard ensures depth/waiting_queue cleanup on any cancellation
        // path (task abort, client disconnect, etc.).
        let mut guard = DrrAdmitGuard::new(
            flow.clone(),
            self.metrics.clone(),
            flow_label.clone(),
            flow_id.clone(),
            self.waiting_queue.clone(),
            self.state.clone(),
        );

        // Create the oneshot channel.
        let (tx, rx) = tokio::sync::oneshot::channel();

        // Push the pending entry into the waiting queue and assign a unique ID.
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
                    _enqueued_at: enter,
                    work_unit,
                });
            // Add to RR cursor if not already present.
            if !state.rr_cursor.iter().any(|id| id == &flow_id) {
                state.rr_cursor.push_back(flow_id.clone());
            }
            my_id
        };
        guard.set_pending_id(pending_id);

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

        Ok(ticket)
    }

    async fn admit_fail_fast(
        &self,
        flow: Arc<Flow>,
        flow_label: String,
        flow_id: FlowId,
        work_unit: f64,
        enter: Instant,
    ) -> Result<QueueTicket, BackpressureRejected> {
        // Check depth BEFORE incrementing (matching Fifo behavior).
        let depth = self.queue_depth();
        if depth > self.max_queue_depth {
            let retry_after =
                fail_fast_retry_after(depth, self.max_queue_depth, self.retry_after_base);
            return Err(BackpressureRejected { retry_after });
        }

        // Proceed with blocking behavior (depth managed inside).
        self.admit_blocking(flow, flow_label, flow_id, work_unit, enter)
            .await
    }

    async fn admit_hybrid(
        &self,
        flow: Arc<Flow>,
        flow_label: String,
        flow_id: FlowId,
        work_unit: f64,
        enter: Instant,
    ) -> Result<QueueTicket, BackpressureRejected> {
        // RAII guard ensures depth/waiting_queue cleanup on any cancellation
        // path (task abort, timeout, etc.).
        let mut guard = DrrAdmitGuard::new(
            flow.clone(),
            self.metrics.clone(),
            flow_label.clone(),
            flow_id.clone(),
            self.waiting_queue.clone(),
            self.state.clone(),
        );

        // Create the oneshot channel.
        let (tx, rx) = tokio::sync::oneshot::channel();

        // Push the pending entry and assign a unique ID.
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
                    _enqueued_at: enter,
                    work_unit,
                });
            // Add to RR cursor if not already present.
            if !state.rr_cursor.iter().any(|id| id == &flow_id) {
                state.rr_cursor.push_back(flow_id.clone());
            }
            my_id
        };
        guard.set_pending_id(pending_id);

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

    /// Return the current credit for the given flow.
    /// Used by tests to verify credit accumulation/consumption.
    pub fn credit(&self, flow_id: &FlowId) -> i64 {
        let flow = self.registry.get_or_create(flow_id.clone());
        flow.credit.load(Ordering::Relaxed)
    }
}

/// RAII guard for DRR blocking-mode admission.
///
/// Created at the start of `admit_blocking` (or `admit_hybrid`) to track
/// the queue depth increment and waiting_queue entry.  If the guard is
/// dropped without being consumed, it decrements depth, removes the
/// waiting_queue entry, and removes the Pending entry from the DRR
/// internal queue (by pending_id — NO sibling-kill).
struct DrrAdmitGuard {
    flow: Arc<Flow>,
    metrics: Arc<Metrics>,
    flow_label: String,
    flow_id: FlowId,
    waiting_queue: Arc<Mutex<VecDeque<FlowId>>>,
    state: Arc<SharedState>,
    active: bool,
    /// The unique ID of this guard's Pending entry. Set to Some after the
    /// Pending is created, so Drop can remove only this entry (not siblings).
    pending_id: Option<u64>,
}

impl DrrAdmitGuard {
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

    /// Set the pending ID after the Pending entry is created.
    fn set_pending_id(&mut self, id: u64) {
        self.pending_id = Some(id);
    }

    /// Called when the request is admitted.  Decrements depth, removes from
    /// waiting queue, and nullifies the guard so Drop is a no-op.
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

impl Drop for DrrAdmitGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        // Decrement depth and update gauge.
        let val = self
            .flow
            .depth
            .fetch_sub(1, Ordering::Relaxed)
            .saturating_sub(1);
        self.metrics
            .queue_depth
            .with_label_values(&[&self.flow_label])
            .set(val as f64);
        // Remove from the public waiting queue.
        remove_from_queue(&self.waiting_queue, &self.flow_id);
        // Remove ONLY this guard's Pending entry from the internal DRR queue.
        // Using pending_id ensures sibling requests are NOT affected.
        // If the pending was already popped by the admission loop, retain is a
        // no-op (the ID won't be found), which is the correct behavior.
        let mut s = self.state.inner.lock().unwrap();
        if let Some(queue) = s.waiting.get_mut(&self.flow_id) {
            if let Some(my_id) = self.pending_id {
                queue.retain(|p| p.pending_id != my_id);
            }
            // If this flow's queue is now empty, reset its credit and remove from cursor.
            if queue.is_empty() {
                s.waiting.remove(&self.flow_id);
                // Reset credit to 0 when queue empties (classic DRR discipline).
                self.flow.credit.store(0, Ordering::Relaxed);
                self.metrics
                    .flow_credit
                    .with_label_values(&[&self.flow_label])
                    .set(0.0);
                // Remove from RR cursor.
                s.rr_cursor.retain(|id| id != &self.flow_id);
            }
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
