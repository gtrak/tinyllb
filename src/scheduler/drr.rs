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
//! DEFICIT (eligibility credit) is reset to 0; permanent `flow.credit`
//! is accounting-only and is not reset (it accumulates debits from
//! selections and restores from cancels/completions).
//!
//! Core architecture:
//! - `Pending`, `SharedState`, admission loop, RAII guard, backpressure,
//!   queue_snapshot, ticket/disarm pattern.
//! - Selection: DRR round-robin cursor with credit bookkeeping.
//!
//! Priority & starvation: `try_select` checks for starved flows first
//! (force-select), then picks the highest-priority eligible flow.
//! Ties broken by round-robin order (base DRR rule).

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::BackpressureMode;
use crate::flow::{Flow, FlowId, FlowRegistry, QueueSnapshot};
use crate::metrics::Metrics;
use crate::scheduler::backpressure::{fail_fast_retry_after, BackpressureRejected};
use crate::scheduler::completion_bias::CompletionBiasGate;
use crate::scheduler::ticket::{make_ticket, QueueTicket};
use crate::scheduler::lifecycle::AccountingReport;
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
    /// When this request was enqueued.
    _enqueued_at: Instant,
    /// Work unit (max_tokens estimate) for this request.
    work_unit: f64,
}

/// Shared mutable state for the DRR scheduler.
struct DrrState {
    /// Per-flow waiting queues. Each queue holds pending requests in FIFO order.
    waiting: std::collections::HashMap<FlowId, VecDeque<Pending>>,
    /// Round-robin cursor: ordered list of flow IDs that have waiting requests.
    /// Flows are removed when their queue empties (their deficit is cleared
    /// in the guard; permanent flow.credit is NOT reset).
    rr_cursor: VecDeque<FlowId>,
    /// Number of available permits (max_active_flows - currently active).
    available_permits: u32,
    /// Monotonically increasing counter for unique pending IDs.
    next_pending_id: u64,
    /// DRR deficit credit accumulated per flow (separate from flow.credit).
    /// Used only for eligibility decisions; cleared when a flow is selected.
    /// This keeps flow.credit clean for accounting (debit at selection,
    /// restore on cancel/completion).
    deficit: std::collections::HashMap<FlowId, i64>,
}

/// Shared state for the DRR scheduler.
struct SharedState {
    inner: Mutex<DrrState>,
    /// Notify the admission loop that something changed (slot freed, new request).
    notify: Arc<tokio::sync::Notify>,
}

/// DRR scheduler with per-flow deficit round-robin credit bookkeeping.
// @lat: [[scheduler#Deficit Round Robin Discipline]]
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
    /// Completion bias gate for pre-admit checks.
    completion_bias_gate: Arc<CompletionBiasGate>,
}

impl DrrScheduler {
    /// Create a new DRR scheduler with policy hooks.
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
        completion_bias_gate: Arc<CompletionBiasGate>,
        kv_bias: Arc<super::kv_bias::KvBiasHandle>,
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
            completion_bias_gate,
            kv_bias,
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
        kv_bias: Arc<super::kv_bias::KvBiasHandle>,
    ) -> Self {
        let notify = Arc::new(tokio::sync::Notify::new());
        let state = Arc::new(SharedState {
            inner: Mutex::new(DrrState {
                waiting: std::collections::HashMap::new(),
                rr_cursor: VecDeque::new(),
                available_permits: max_active_flows,
                next_pending_id: 0,
                deficit: std::collections::HashMap::new(),
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
            kv_bias.clone(),
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

    /// Background admission loop.
    async fn admission_loop(
        state: Arc<SharedState>,
        metrics: Arc<Metrics>,
        registry: Arc<FlowRegistry>,
        starvation_timeout: Duration,
        gate: Arc<CompletionBiasGate>,
        kv_bias: Arc<super::kv_bias::KvBiasHandle>,
    ) {
        loop {
            state.notify.notified().await;

            loop {
                let (has_permits, has_waiting) = {
                    let s = state.inner.lock().unwrap();
                    (s.available_permits > 0, !s.rr_cursor.is_empty())
                };
                if !has_permits || !has_waiting {
                    break;
                }

                let (selection, credit_accumulated) =
                    Self::try_select(&state, &registry, &metrics, starvation_timeout, &kv_bias);
                match selection {
                    None => {
                        if credit_accumulated {
                            // No flow eligible this round, but credit was accumulated.
                        } else {
                            break;
                        }
                    }
                    Some((flow_id, pending, work_unit)) => {
                        let metrics_clone = metrics.clone();
                        let flow_id_for_ticket = flow_id.clone();
                        let state_clone = state.clone();
                        let gate_clone = gate.clone();
                        let flow_for_active = registry.get_or_create(flow_id.clone());

                        let ticket =
                            make_ticket(flow_id_for_ticket.clone(), work_unit, move || {
                                metrics_clone.active_flows.dec();
                                flow_for_active.dec_active();

                                {
                                    let mut s = state_clone.inner.lock().unwrap();
                                    s.available_permits += 1;
                                }

                                state_clone.notify.notify_one();
                                gate_clone.notify_waiters();
                            });

                        match pending.tx.send(ticket) {
                            Ok(()) => {}
                            Err(mut ticket) => {
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
    /// Selection order:
    /// 1. Starved flows are force-selected first (bypassing credit check).
    /// 2. Among eligible flows (credit >= cost), highest priority wins.
    /// 3. Ties broken by round-robin order (base DRR rule).
    fn try_select(
        state: &Arc<SharedState>,
        registry: &FlowRegistry,
        metrics: &Metrics,
        starvation_timeout: Duration,
        kv_bias: &Arc<super::kv_bias::KvBiasHandle>,
    ) -> (Option<(FlowId, Pending, f64)>, bool) {
        let mut s = state.inner.lock().unwrap();
        if s.available_permits == 0 {
            return (None, false);
        }

        if s.rr_cursor.is_empty() {
            return (None, false);
        }

        // Phase 1: Check for starved flows (force-select if found).
        // Collect candidate flow IDs first to avoid borrow conflicts.
        let starved_candidates: Vec<FlowId> = s
            .rr_cursor
            .iter()
            .filter(|fid| s.waiting.get(*fid).is_some_and(|q| !q.is_empty()))
            .cloned()
            .collect();

        for fid in &starved_candidates {
            let flow = registry.get_or_create(fid.clone());
            if let Some(wait) = starvation::is_starved(&flow, starvation_timeout) {
                // Force-select this starved flow (bypass credit check).
                starvation::record_force_admit(metrics, &flow, wait);

                let selected = fid.clone();

                let pending = s
                    .waiting
                    .get_mut(&selected)
                    .and_then(|q| q.pop_front())
                    .expect("starved flow should have pending entry");
                let work_unit = pending.work_unit;

                // Deduct cost from credit (force, even if credit was insufficient).
                let new_credit = flow.credit.load(Ordering::Relaxed) - (work_unit as i64);
                flow.credit.store(new_credit, Ordering::Relaxed);
                metrics
                    .flow_credit
                    .with_label_values(&[selected.metric_label()])
                    .set(new_credit as f64);

                // If this flow's queue is now empty, clean up the waiting map.
                if s.waiting.get(&selected).is_none_or(|q| q.is_empty()) {
                    s.waiting.remove(&selected);
                    // NOTE: Do NOT reset credit to 0 here. The selection-time
                    // debit already reduced credit by the work_unit. The restore
                    // on cancel/completion needs to work against that debited
                    // value, not a zeroed baseline.
                }

                // Rotate cursor.
                s.rr_cursor.retain(|id| id != &selected);
                if s.waiting.get(&selected).is_some_and(|q| !q.is_empty()) {
                    s.rr_cursor.push_back(selected.clone());
                }

                // Clear enqueued_at.
                {
                    let mut enq = flow.enqueued_at.write().unwrap();
                    *enq = None;
                }

                s.available_permits -= 1;
                drop(s);

                return (Some((selected.clone(), pending, work_unit)), true);
            }
        }
        // Drop unused idx to avoid borrow conflicts.
        drop(starved_candidates);

        // Phase 2: Accumulate deficit credit for ALL waiting flows in this round.
        // Deficit is tracked separately from flow.credit — flow.credit is the
        // permanent accounting balance (modified only by debit at selection and
        // restore on cancel/completion). The deficit is purely for DRR eligibility.
        let mut eligible_flows: Vec<(usize, FlowId)> = Vec::new();
        let mut credit_accumulated = false;

        // Collect cursor entries first to avoid borrow conflicts with deficit map.
        let cursor_entries: Vec<(usize, FlowId)> = s
            .rr_cursor
            .iter()
            .enumerate()
            .map(|(i, f)| (i, f.clone()))
            .collect();

        for (idx, flow_id) in cursor_entries {
            // Get cost from the queue BEFORE mutating the deficit map.
            let cost_i64 = match s.waiting.get(&flow_id) {
                Some(q) if q.is_empty() => {
                    continue;
                }
                Some(q) => q.front().expect("queue should not be empty").work_unit as i64,
                None => continue,
            };

            let flow = registry.get_or_create(flow_id.clone());
            let weight_i64 = flow.weight() as i64;
            if weight_i64 <= 0 {
                continue;
            }

            // Accumulate into deficit (not flow.credit).
            let deficit_entry = s.deficit.entry(flow_id.clone()).or_insert(0);
            *deficit_entry += weight_i64;
            credit_accumulated = true;

            // Update the flow_credit metric to reflect total (permanent + deficit).
            let permanent = flow.credit.load(Ordering::Relaxed);
            metrics
                .flow_credit
                .with_label_values(&[flow_id.metric_label()])
                .set((permanent + *deficit_entry) as f64);

            if *deficit_entry >= cost_i64 {
                eligible_flows.push((idx, flow_id.clone()));
            }
        }

        // Phase 3: Among eligible flows, select highest priority (tie-break: RR order).
        if !eligible_flows.is_empty() {
            // Build candidates for priority selection.
            let candidates: Vec<priority::FlowCandidate> = eligible_flows
                .iter()
                .map(|(idx, fid)| {
                    let flow = registry.get_or_create(fid.clone());
                    let head = s.waiting.get(fid).and_then(|q| q.front()).unwrap();
                    priority::FlowCandidate {
                        flow_id: fid.clone(),
                        priority: flow.priority(),
                        enqueued_at: head._enqueued_at,
                        base_score: *idx as f64, // Lower index = earlier in RR = preferred
                        kv_footprint: kv_bias.footprint(fid),
                    }
                })
                .collect();

            let pressure = kv_bias.pressure();
            let selected_flow_id =
                kv_bias.select(&candidates, pressure).expect("candidates should not be empty");

            // Find the selected flow in eligible list.
            let (_selected_idx, _) = eligible_flows
                .iter()
                .find(|(_, fid)| fid == &selected_flow_id)
                .expect("selected should be in eligible list");

            let pending = s
                .waiting
                .get_mut(&selected_flow_id)
                .and_then(|q| q.pop_front())
                .expect("eligible flow should have pending entry");
            let work_unit = pending.work_unit;

            // Deduct cost from PERMANENT credit (flow.credit), not deficit.
            // The deficit was only used for eligibility; now clear it.
            s.deficit.remove(&selected_flow_id);

            let flow = registry.get_or_create(selected_flow_id.clone());
            let new_credit = flow.credit.load(Ordering::Relaxed) - (work_unit as i64);
            flow.credit.store(new_credit, Ordering::Relaxed);
            metrics
                .flow_credit
                .with_label_values(&[selected_flow_id.metric_label()])
                .set(new_credit as f64);

            // If this flow's queue is now empty, clean up the waiting map.
            if s.waiting
                .get(&selected_flow_id)
                .is_none_or(|q| q.is_empty())
            {
                s.waiting.remove(&selected_flow_id);
                // NOTE: Do NOT reset credit to 0 here. The selection-time
                // debit already reduced credit by the work_unit. The restore
                // on cancel/completion needs to work against that debited
                // value, not a zeroed baseline.
            }

            // Rotate: move served flow to the back of cursor (if still has requests).
            s.rr_cursor.retain(|id| id != &selected_flow_id);
            if s.waiting
                .get(&selected_flow_id)
                .is_some_and(|q| !q.is_empty())
            {
                s.rr_cursor.push_back(selected_flow_id.clone());
            }

            // Clear enqueued_at.
            {
                let mut enq = flow.enqueued_at.write().unwrap();
                *enq = None;
            }

            s.available_permits -= 1;
            drop(s);

            return (Some((selected_flow_id, pending, work_unit)), true);
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
            s.deficit.remove(fid);
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
        let mut guard = DrrAdmitGuard::new(
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

        let (tx, rx) = tokio::sync::oneshot::channel();

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
            if !state.rr_cursor.iter().any(|id| id == &flow_id) {
                state.rr_cursor.push_back(flow_id.clone());
            }
            my_id
        };
        guard.set_pending_id(pending_id);

        // Set enqueued_at for starvation detection.
        {
            let mut enq = flow.enqueued_at.write().unwrap();
            *enq = Some(enter);
        }

        self.state.notify.notify_one();

        let ticket = rx.await.map_err(|_| BackpressureRejected {
            retry_after: std::time::Duration::from_secs(1),
        })?;

        guard.consume();

        let wait_secs = enter.elapsed().as_secs_f64();
        self.metrics.queue_wait_seconds.observe(wait_secs);
        self.metrics.active_flows.inc();
        flow.inc_active();

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
        let depth = self.queue_depth();
        if depth > self.max_queue_depth {
            let retry_after =
                fail_fast_retry_after(depth, self.max_queue_depth, self.retry_after_base);
            return Err(BackpressureRejected { retry_after });
        }

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
        let mut guard = DrrAdmitGuard::new(
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

        let (tx, rx) = tokio::sync::oneshot::channel();

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
            if !state.rr_cursor.iter().any(|id| id == &flow_id) {
                state.rr_cursor.push_back(flow_id.clone());
            }
            my_id
        };
        guard.set_pending_id(pending_id);

        // Set enqueued_at for starvation detection.
        {
            let mut enq = flow.enqueued_at.write().unwrap();
            *enq = Some(enter);
        }

        self.state.notify.notify_one();

        let result = tokio::select!(
            biased;
            ticket = rx => {
                match ticket {
                    Ok(t) => {
                        guard.consume();
                        let wait_secs = enter.elapsed().as_secs_f64();
                        self.metrics.queue_wait_seconds.observe(wait_secs);
                        self.metrics.active_flows.inc();
                        flow.inc_active();
                        Ok(t)
                    }
                    Err(_) => {
                        Err(BackpressureRejected {
                            retry_after: std::time::Duration::from_secs(1),
                        })
                    }
                }
            }
            _ = tokio::time::sleep(self.max_wait) => {
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
    pub fn credit(&self, flow_id: &FlowId) -> i64 {
        let flow = self.registry.get_or_create(flow_id.clone());
        flow.credit.load(Ordering::Relaxed)
    }

    /// Report accounting for a completed or cancelled request.
    ///
    /// On completion: restore `estimated - delivered` so net credit = -delivered.
    /// On cancel: restore `estimated - delivered` so net credit = `-delivered`.
    pub fn report_accounting(&self, flow_id: &FlowId, report: AccountingReport) {
        let flow = self.registry.get_or_create(flow_id.clone());
        let flow_label = flow_id.metric_label().to_string();
        match report {
            AccountingReport::Completed {
                delivered_tokens: _,
                restore_cost,
            } => {
                // Restore estimated - delivered.
                let current = flow.credit.load(Ordering::Relaxed);
                let new_credit = current + restore_cost;
                flow.credit.store(new_credit, Ordering::Relaxed);
                self.metrics
                    .flow_credit
                    .with_label_values(&[&flow_label])
                    .set(new_credit as f64);
            }
            AccountingReport::Cancelled { restore_cost } => {
                // Restore estimated - delivered.
                let current = flow.credit.load(Ordering::Relaxed);
                let new_credit = current + restore_cost;
                flow.credit.store(new_credit, Ordering::Relaxed);
                self.metrics
                    .flow_credit
                    .with_label_values(&[&flow_label])
                    .set(new_credit as f64);
            }
        }
    }
}

/// RAII guard for DRR blocking-mode admission.
struct DrrAdmitGuard {
    flow: Arc<Flow>,
    metrics: Arc<Metrics>,
    flow_label: String,
    flow_id: FlowId,
    waiting_queue: Arc<Mutex<VecDeque<FlowId>>>,
    state: Arc<SharedState>,
    active: bool,
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

impl Drop for DrrAdmitGuard {
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
                // Clear the flow's deficit (it's no longer waiting).
                // Do NOT reset permanent credit — it was never debited
                // (the flow was never admitted), so it should stay as-is.
                s.deficit.remove(&self.flow_id);
                s.rr_cursor.retain(|id| id != &self.flow_id);
                // Update metrics to reflect permanent credit only (no deficit).
                let permanent = self.flow.credit.load(Ordering::Relaxed);
                self.metrics
                    .flow_credit
                    .with_label_values(&[&self.flow_label])
                    .set(permanent as f64);
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
