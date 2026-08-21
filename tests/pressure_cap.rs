//! KV-pressure concurrency cap — end-to-end tests (plan 008, task 04).
//!
//! Drives `Scheduler::new` with an injected backend monitor
//! (`BackendMonitor::from_receiver`) and the test ladder
//! `{0.5 -> 3, 0.8 -> 2, 0.95 -> 1}` with `max_active_flows: 4`.
//! Requests are fired via direct `scheduler.admit(...)`; holding the returned
//! ticket keeps the flow active, dropping it completes the request. This
//! avoids the HTTP layer while exercising the real admission loop, the
//! snapshot wake arm, and the permit accounting.
//!
//! The KV admission *gate* (`KvPolicyConfig`) is left disabled so these tests
//! isolate the pressure cap; the cap is a soft ceiling on NEW admissions and
//! never aborts in-flight requests.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tinyllb::backend::{BackendMonitor, BackendSnapshot};
use tinyllb::config::{
    BackpressureMode, KvBias, KvPolicyConfig, KvPressure, KvPressureThreshold, Priorities,
    PriorityPolicy,
};
use tinyllb::flow::{FlowId, FlowRegistry};
use tinyllb::metrics;
use tinyllb::scheduler::{
    BackpressureRejected, FlowProgressTracker, KvBiasHandle, QueueTicket, Scheduler,
};

const MAX_ACTIVE_FLOWS: u32 = 4;
const WORK_UNIT: f64 = 1024.0;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The ladder used by all end-to-end tests: `{0.5 -> 3, 0.8 -> 2, 0.95 -> 1}`.
fn test_ladder() -> KvPressure {
    KvPressure {
        enabled: true,
        thresholds: vec![
            KvPressureThreshold {
                at: 0.5,
                max_flows: 3,
            },
            KvPressureThreshold {
                at: 0.8,
                max_flows: 2,
            },
            KvPressureThreshold {
                at: 0.95,
                max_flows: 1,
            },
        ],
    }
}

fn snapshot(kv_usage: f64) -> BackendSnapshot {
    BackendSnapshot {
        kv_usage,
        kv_free: 1.0 - kv_usage,
        ..Default::default()
    }
}

/// Scheduler with `max_active_flows: 4`, `BackpressureMode::Blocking`, the
/// KV admission gate disabled, default completion bias / priority / kv bias,
/// and the given monitor + `KvPressure` config.
fn build_scheduler(
    kv_pressure: KvPressure,
    monitor: Arc<BackendMonitor>,
) -> (Arc<Scheduler>, Arc<metrics::Metrics>) {
    let m = metrics::create_metrics();
    let registry = Arc::new(FlowRegistry::new(1.0, 50));
    let scheduler = Arc::new(Scheduler::new(
        MAX_ACTIVE_FLOWS,
        m.clone(),
        registry,
        BackpressureMode::Blocking,
        100,
        Duration::from_secs(10),
        Duration::from_secs(1),
        Duration::from_secs(300),
        Default::default(), // completion_bias
        KvPolicyConfig::default(), // KV gate disabled — isolate the cap
        monitor,
        PriorityPolicy::default(),
        Priorities::default(),
        KvBias::default(),
        kv_pressure,
    ));
    (scheduler, m)
}

/// An admit fired on a background task whose ticket is handed back to the
/// test, so the test controls when the flow "completes" (ticket drop).
struct PendingAdmit {
    ticket: Option<tokio::sync::oneshot::Receiver<Result<QueueTicket, BackpressureRejected>>>,
    done: tokio::sync::watch::Receiver<bool>,
}

impl PendingAdmit {
    /// Whether the admit has resolved (ticket available or error).
    fn is_done(&self) -> bool {
        *self.done.borrow()
    }

    /// Consume the admitted ticket (panics if not done or the admit errored).
    fn take_ticket(&mut self) -> QueueTicket {
        self.ticket
            .take()
            .expect("take_ticket called twice")
            .try_recv()
            .expect("admit result was sent")
            .expect("admit should succeed")
    }

    /// Take and drop the ticket (completes the flow). No-op if already taken.
    fn complete(&mut self) {
        if let Some(mut rx) = self.ticket.take() {
            let _ticket = rx
                .try_recv()
                .expect("admit result was sent")
                .expect("admit should succeed");
        }
    }
}

fn spawn_admit(scheduler: Arc<Scheduler>, name: &str) -> PendingAdmit {
    let name = name.to_string();
    let s = scheduler.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (done_tx, done_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        let result = s.admit(FlowId::new(name), WORK_UNIT).await;
        let _ = tx.send(result);
        let _ = done_tx.send(true);
    });
    PendingAdmit {
        ticket: Some(rx),
        done: done_rx,
    }
}

/// Poll (bounded to 2s) until `condition` holds; sleep-based so the
/// scheduler's admission loop gets to run.
async fn wait_until(label: &str, mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        if start.elapsed() > Duration::from_secs(2) {
            panic!("timed out waiting for: {label}");
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

fn active(m: &Arc<metrics::Metrics>) -> f64 {
    m.active_flows.get()
}

async fn wait_for_active(m: &Arc<metrics::Metrics>, expected: f64) {
    wait_until(&format!("active_flows == {expected}"), || active(m) == expected).await;
}

// ---------------------------------------------------------------------------
// End-to-end cap tests (tests/pressure_cap.rs, plan 008)
// ---------------------------------------------------------------------------

/// Regression guard: below the first threshold (0.2 < 0.5) the cap must not
/// reduce `max_active_flows` — all 4 slots are usable (peak active == 4).
#[tokio::test]
async fn low_pressure_full_concurrency() {
    let (_tx, rx) = tokio::sync::watch::channel(snapshot(0.2));
    let monitor = Arc::new(BackendMonitor::from_receiver(rx));
    let (scheduler, m) = build_scheduler(test_ladder(), monitor);

    let mut pending: Vec<PendingAdmit> = (0..4)
        .map(|i| spawn_admit(scheduler.clone(), &format!("flow-{i}")))
        .collect();

    wait_until("all 4 admits granted", || {
        pending.iter().all(|p| p.is_done())
    })
    .await;

    assert_eq!(
        active(&m),
        4.0,
        "peak active must reach max_active_flows=4 at low pressure"
    );

    for p in &mut pending {
        let _ticket = p.take_ticket(); // drop completes the flow
    }
    wait_for_active(&m, 0.0).await;
    assert_eq!(scheduler.queue_depth(), 0);
}

/// At pressure 0.9 (band cap = 2) set BEFORE any admit, 5 concurrent admits
/// grant exactly 2 tickets promptly; the other 3 remain awaiting and visible
/// in the queue.
///
/// This also covers the plan's `vllm_style_snapshot_uses_same_ladder`
/// variant: the cap reads only `kv_usage` off the monitor's
/// `BackendSnapshot`, which carries no signal-source label — the same struct
/// is filled from either the vLLM or the llama.cpp `/metrics` endpoint, so
/// the ladder is signal-source agnostic and this single test pins both.
#[tokio::test]
async fn high_pressure_holds_at_cap() {
    let (tx, rx) = tokio::sync::watch::channel(snapshot(0.9));
    let monitor = Arc::new(BackendMonitor::from_receiver(rx));
    let (scheduler, m) = build_scheduler(test_ladder(), monitor);

    let mut pending: Vec<PendingAdmit> = (0..5)
        .map(|i| spawn_admit(scheduler.clone(), &format!("flow-{i}")))
        .collect();

    // Exactly cap(0.9) = 2 tickets granted promptly.
    wait_until("2 admits granted", || {
        pending.iter().filter(|p| p.is_done()).count() == 2
    })
    .await;
    assert_eq!(active(&m), 2.0, "active must equal the band cap of 2");

    // The other 3 remain awaiting (no grant beyond the cap).
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        pending.iter().filter(|p| p.is_done()).count(),
        2,
        "no admit may be granted beyond the cap"
    );
    assert_eq!(
        scheduler.queue_depth(),
        3,
        "the 3 held requests must be visible in the queue"
    );

    // Cleanup: lift the cap, then drop the granted tickets so the
    // waiting ones drain.
    let _ = tx.send(snapshot(0.2));
    for p in &mut pending {
        if p.is_done() {
            p.complete();
        }
    }
    wait_until("all 5 admits granted after cap cleared", || {
        pending.iter().all(|p| p.is_done())
    })
    .await;
    for p in &mut pending {
        p.complete();
    }
    wait_for_active(&m, 0.0).await;
}

/// Pressure 0.2 -> admit 4. Raise to 0.9 (cap 2): the 5th request fired
/// while high is held even after active drains to 3, and even when active ==
/// cap (2). Only when active drops to 1 (< cap) does the 5th admit, refilling
/// to 2 — not 3 (a 6th request is held as well).
#[tokio::test]
async fn cap_drop_drains_then_holds() {
    let (tx, rx) = tokio::sync::watch::channel(snapshot(0.2));
    let monitor = Arc::new(BackendMonitor::from_receiver(rx));
    let (scheduler, m) = build_scheduler(test_ladder(), monitor);

    // Low pressure: all 4 slots granted.
    let mut holders: Vec<PendingAdmit> = (0..4)
        .map(|i| spawn_admit(scheduler.clone(), &format!("flow-{i}")))
        .collect();
    wait_until("all 4 holders admitted", || {
        holders.iter().all(|p| p.is_done())
    })
    .await;
    // Hold the 4 tickets — a held ticket keeps its flow active.
    let mut tickets: Vec<QueueTicket> =
        holders.iter_mut().map(|p| p.take_ticket()).collect();
    wait_for_active(&m, 4.0).await;

    // Pressure rises to cap 2; in-flight tickets are unaffected.
    let _ = tx.send(snapshot(0.9));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(active(&m), 4.0, "in-flight flows are never preempted");

    // 5th request fired while pressure high — must not be admitted.
    let mut fifth = spawn_admit(scheduler.clone(), "flow-5");
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(!fifth.is_done(), "5th must be held: active 4 >= cap 2");

    // Complete 1 -> active 3 (>= cap 2): 5th still held.
    tickets.pop().expect("a held ticket to complete");
    wait_for_active(&m, 3.0).await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(!fifth.is_done(), "5th must be held: active 3 >= cap 2");

    // Complete 1 more -> active 2 (== cap 2): 5th STILL held.
    tickets.pop().expect("a held ticket to complete");
    wait_for_active(&m, 2.0).await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(!fifth.is_done(), "5th must be held: active 2 == cap 2");

    // Complete 1 more -> active 1 (< cap 2): 5th refills the slot.
    tickets.pop().expect("a held ticket to complete");
    wait_for_active(&m, 1.0).await;
    wait_until("5th admitted", || fifth.is_done()).await;
    let ticket5 = fifth.take_ticket();
    wait_for_active(&m, 2.0).await;
    assert_eq!(active(&m), 2.0, "refill stops at cap 2, not 3");

    // A 6th request is also held at cap 2.
    let mut sixth = spawn_admit(scheduler.clone(), "flow-6");
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(!sixth.is_done(), "6th must be held at cap 2");
    assert_eq!(active(&m), 2.0);

    // Cleanup: complete the 5th and last holder, lift the cap, drain the 6th.
    drop(ticket5);
    drop(tickets); // the 4th holder
    let _ = tx.send(snapshot(0.2));
    wait_until("6th admitted after cleanup", || sixth.is_done()).await;
    {
        let _ticket = sixth.take_ticket();
    }
    wait_for_active(&m, 0.0).await;
    assert_eq!(scheduler.queue_depth(), 0);
}

/// The snapshot-wake proof: at cap 1 the 2nd request awaits with NO
/// completions in flight; a pressure drop alone (watch `changed()` arm of the
/// admission loop) must grant its ticket. A notify-only loop would hang here.
#[tokio::test]
async fn pressure_drop_reopens_without_completion() {
    let (tx, rx) = tokio::sync::watch::channel(snapshot(0.95));
    let monitor = Arc::new(BackendMonitor::from_receiver(rx));
    let (scheduler, m) = build_scheduler(test_ladder(), monitor);

    // cap(0.95) = 1: the first request takes the only slot.
    let mut first = spawn_admit(scheduler.clone(), "flow-1");
    wait_until("first admitted", || first.is_done()).await;
    let ticket1 = first.take_ticket();
    wait_for_active(&m, 1.0).await;

    // 2nd request awaits: active 1 >= cap 1.
    let mut second = spawn_admit(scheduler.clone(), "flow-2");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!second.is_done(), "2nd must await: active 1 >= cap(0.95) = 1");

    // No completion — a pressure drop alone must reopen admission.
    let _ = tx.send(snapshot(0.2));
    wait_until("2nd admitted after pressure drop (no completion)", || second.is_done())
        .await;
    let ticket2 = second.take_ticket();
    wait_for_active(&m, 2.0).await;

    drop(ticket1);
    drop(ticket2);
    wait_for_active(&m, 0.0).await;
}

/// In-flight requests are never aborted by a pressure spike: 4 active flows
/// at cap-1 pressure all complete normally on ticket drop, and the queue
/// drains to 0.
#[tokio::test]
async fn in_flight_never_aborted() {
    let (tx, rx) = tokio::sync::watch::channel(snapshot(0.2));
    let monitor = Arc::new(BackendMonitor::from_receiver(rx));
    let (scheduler, m) = build_scheduler(test_ladder(), monitor);

    let mut holders: Vec<PendingAdmit> = (0..4)
        .map(|i| spawn_admit(scheduler.clone(), &format!("flow-{i}")))
        .collect();
    wait_until("all 4 holders admitted", || {
        holders.iter().all(|p| p.is_done())
    })
    .await;
    wait_for_active(&m, 4.0).await;

    // Cap slams to 1 — in-flight tickets must remain valid.
    let _ = tx.send(snapshot(0.95));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        active(&m),
        4.0,
        "in-flight flows are never aborted by the cap"
    );

    // Each ticket completes normally (drop releases its permit).
    for p in &mut holders {
        let _ticket = p.take_ticket();
    }
    wait_for_active(&m, 0.0).await;
    assert_eq!(scheduler.queue_depth(), 0, "queue must drain to 0");
}

/// `KvPressure::default()` (disabled) is inert: at pressure 0.99 all 4
/// concurrent admits are granted (peak active == 4). Pins the default-off
/// contract at the scheduler level.
#[tokio::test]
async fn disabled_is_inert() {
    let (_tx, rx) = tokio::sync::watch::channel(snapshot(0.99));
    let monitor = Arc::new(BackendMonitor::from_receiver(rx));
    let (scheduler, m) = build_scheduler(KvPressure::default(), monitor);

    let mut pending: Vec<PendingAdmit> = (0..4)
        .map(|i| spawn_admit(scheduler.clone(), &format!("flow-{i}")))
        .collect();
    wait_until("all 4 admits granted with disabled cap", || {
        pending.iter().all(|p| p.is_done())
    })
    .await;
    assert_eq!(
        active(&m),
        4.0,
        "a disabled cap must not limit concurrency"
    );

    for p in &mut pending {
        let _ticket = p.take_ticket();
    }
    wait_for_active(&m, 0.0).await;
}

// ---------------------------------------------------------------------------
// kv_bias ramp pinning (weight level)
//
// `KvBiasHandle` is public and constructible from the test crate, so the
// ramp is pinned here directly. The `select`-level pins (disabled weight,
// footprint preference) live in an in-file `#[cfg(test)]` module in
// `src/scheduler/kv_bias.rs` because `FlowCandidate` is not exported.
// ---------------------------------------------------------------------------

fn bias_handle(config: KvBias) -> KvBiasHandle {
    KvBiasHandle::new(
        config,
        Arc::new(BackendMonitor::empty()),
        Arc::new(FlowProgressTracker::new()),
    )
}

/// Default ramp 0.5 -> 0.9: at/above `bias_full_at` the weight is inclusive
/// full (1.0).
#[test]
fn bias_weight_high_pressure_full() {
    let h = bias_handle(KvBias::default());
    assert_eq!(h.bias_weight(0.95), 1.0);
    assert_eq!(
        h.bias_weight(0.9),
        1.0,
        "weight must be inclusive-full at bias_full_at"
    );
}

/// Mid-ramp: linear interpolation (0.7 - 0.5) / (0.9 - 0.5) = 0.5.
#[test]
fn bias_weight_midpoint() {
    let h = bias_handle(KvBias::default());
    assert!(
        (h.bias_weight(0.7) - 0.5).abs() < 1e-9,
        "midpoint weight should be 0.5, got {}",
        h.bias_weight(0.7)
    );
}

/// Below `pressure_below` — and inclusive AT it — the weight is 0.0 (pure
/// fairness).
#[test]
fn bias_weight_below_pressure_below_zero() {
    let h = bias_handle(KvBias::default());
    assert_eq!(h.bias_weight(0.3), 0.0);
    assert_eq!(
        h.bias_weight(0.5),
        0.0,
        "weight must be inclusive-zero at pressure_below"
    );
}
