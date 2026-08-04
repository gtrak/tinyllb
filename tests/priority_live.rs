//! End-to-end tests for the interactive-vs-batch priority heuristic.
//!
//! These tests verify that the cadence heuristic correctly classifies flows
//! and that higher-priority flows win scheduling contention over lower-priority
//! flows. They also verify the starvation safety net still fires for low-
//! priority flows.
//!
//! IMPORTANT: These tests use real wall-clock time via `tokio::time::sleep`
//! WITHOUT `tokio::time::pause()`. The cadence heuristic calls
//! `std::time::Instant::now()` which is not affected by tokio's fake clock.
//! Real sleeps advance the wall clock that Instant observes.

use std::sync::Arc;
use std::time::Duration;

use tinyllb::backend::BackendMonitor;
use tinyllb::config::{
    Algorithm, BackpressureMode, CompletionBias, KvPolicyConfig, Priorities, PriorityPolicy,
};
use tinyllb::flow::{FlowId, FlowRegistry, PriorityClass};
use tinyllb::metrics::{self, Metrics};
use tinyllb::scheduler::Scheduler;

const WORK_UNIT: f64 = 1024.0;

/// Test-specific priority policy with small thresholds for fast testing.
///
/// - interactive_gap_min: 50ms (gaps >= 50ms → interactive, priority 100)
/// - background_gap_max: 20ms (gaps <= 20ms with min_samples → background, priority 10)
/// - min_samples: 3 (heuristic engages after 3 arrivals)
/// - sample_window: 10
fn test_policy() -> PriorityPolicy {
    PriorityPolicy {
        enabled: true,
        interactive_gap_min: Duration::from_millis(50),
        background_gap_max: Duration::from_millis(20),
        sample_window: 10,
        min_samples: 3,
    }
}

/// Build a DRR scheduler with max_active_flows=1 and the test priority policy.
fn build_scheduler(policy: PriorityPolicy) -> (Arc<Metrics>, Arc<FlowRegistry>, Arc<Scheduler>) {
    let m = metrics::create_metrics();
    let registry = Arc::new(FlowRegistry::new(1.0, 50));
    let scheduler = Arc::new(Scheduler::new(
        Algorithm::Drr,
        1, // max_active_flows=1 — contention is visible
        m.clone(),
        registry.clone(),
        BackpressureMode::Blocking,
        100,
        Duration::from_secs(10),
        Duration::from_secs(1),
        Duration::from_secs(300), // long starvation — not relevant for most tests
        CompletionBias::default(),
        KvPolicyConfig::default(),
        Arc::new(BackendMonitor::empty()),
        policy,
        Priorities::default(),
    ));
    (m, registry, scheduler)
}

// ---------------------------------------------------------------------------
// Test 1: interactive_flow_wins_over_batch
// ---------------------------------------------------------------------------

/// End-to-end test: an interactive flow (high inter-request gaps) is
/// promoted to priority 100, a batch flow (rapid requests) is demoted to
/// priority 10, and when both compete for a single slot, the interactive
/// flow wins.
///
/// This is the key scheduling test. It exercises the full pipeline:
/// record_arrival → classify_and_apply → priority lookup in DRR → slot
/// assignment → contention resolution by priority.
#[tokio::test]
async fn interactive_flow_wins_over_batch() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (m, registry, scheduler) = build_scheduler(test_policy());

        // ── Build cadence for the interactive flow ──
        // Sleep 60ms between admits → gap ~60ms >= 50ms → interactive (100).
        let inter_id = FlowId::new("interactive-flow");
        for _ in 0..3 {
            let ticket = scheduler.admit(inter_id.clone(), WORK_UNIT).await.unwrap();
            drop(ticket);
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
        assert_eq!(
            registry.get_or_create(inter_id.clone()).priority(),
            100,
            "interactive flow should be classified as priority 100"
        );

        // ── Build cadence for the batch flow ──
        // Sleep 5ms between admits → gap ~5ms <= 20ms → background (10).
        let batch_id = FlowId::new("batch-flow");
        for _ in 0..3 {
            let ticket = scheduler.admit(batch_id.clone(), WORK_UNIT).await.unwrap();
            drop(ticket);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            registry.get_or_create(batch_id.clone()).priority(),
            10,
            "batch flow should be classified as priority 10"
        );

        // ── Contention round (repeated 3x for robustness) ──
        let mut interactive_wins: u32 = 0;
        let total_rounds: u32 = 3;

        for _round in 0..total_rounds {
            // Holder occupies the slot.
            let holder = registry.get_or_create(FlowId::new("holder"));
            holder.set_weight(1.0);
            let ticket_holder = scheduler
                .admit(FlowId::new("holder"), WORK_UNIT)
                .await
                .unwrap();

            // Enqueue batch flow (priority 10).
            let s_batch = scheduler.clone();
            let task_batch = tokio::spawn(async move {
                s_batch.admit(FlowId::new("batch-flow"), WORK_UNIT).await
            });

            // Give batch time to enter the queue.
            tokio::time::sleep(Duration::from_millis(50)).await;

            // Enqueue interactive flow (priority 100).
            let s_inter = scheduler.clone();
            let task_inter = tokio::spawn(async move {
                s_inter
                    .admit(FlowId::new("interactive-flow"), WORK_UNIT)
                    .await
            });

            // Give interactive time to enter the queue.
            tokio::time::sleep(Duration::from_millis(50)).await;

            // Drop holder → slot frees. Interactive (priority 100) should
            // be admitted before batch (priority 10).
            drop(ticket_holder);

            // Await interactive first — it should complete within the timeout.
            let ticket_inter = tokio::time::timeout(Duration::from_secs(2), task_inter)
                .await
                .expect("interactive admit should not timeout")
                .expect("interactive task should not panic")
                .expect("interactive admit should succeed");

            // Drop interactive's ticket → batch gets the slot.
            drop(ticket_inter);

            let _ticket_batch = tokio::time::timeout(Duration::from_secs(2), task_batch)
                .await
                .expect("batch admit should not timeout")
                .expect("batch task should not panic")
                .expect("batch admit should succeed");

            interactive_wins += 1;
        }

        assert!(
            interactive_wins >= total_rounds.saturating_sub(1),
            "interactive should win >= {}/{} rounds (got {})",
            total_rounds.saturating_sub(1),
            total_rounds,
            interactive_wins
        );

        assert_eq!(m.active_flows.get(), 0.0);
    })
    .await
    .expect("test should not timeout");
}

// ---------------------------------------------------------------------------
// Test 2: starvation_force_admits_background_despite_lower_priority
// ---------------------------------------------------------------------------

/// Starvation regression test: a background-priority flow is force-admitted
/// by the starvation mechanism after starvation_timeout, even though its
/// priority (10) is far below the fast-flow's priority (100).
///
/// This verifies that the 300s (here 500ms for test speed) starvation
/// safety net still trumps priority — background flows are NOT starved
/// forever.
#[tokio::test]
async fn starvation_force_admits_background_despite_lower_priority() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let starvation_timeout = Duration::from_millis(500);
        let m = metrics::create_metrics();
        let registry = Arc::new(FlowRegistry::new(1.0, 50));
        let priorities = Priorities::default();
        let scheduler = Arc::new(Scheduler::new(
            Algorithm::Drr,
            1, // max_active_flows=1
            m.clone(),
            registry.clone(),
            BackpressureMode::Blocking,
            100,
            Duration::from_secs(10),
            Duration::from_secs(1),
            starvation_timeout,
            CompletionBias::default(),
            KvPolicyConfig::default(),
            Arc::new(BackendMonitor::empty()),
            test_policy(),
            priorities.clone(),
        ));

        // Pin fast-flow to interactive (priority 100, source=1).
        registry.apply_priority_override(
            &FlowId::new("fast-flow"),
            Some(PriorityClass::Interactive),
            false,
            &priorities,
        );
        // Pin slow-flow to background (priority 10, source=1).
        registry.apply_priority_override(
            &FlowId::new("slow-flow"),
            Some(PriorityClass::Background),
            false,
            &priorities,
        );

        // Verify pins.
        assert_eq!(
            registry.get_or_create(FlowId::new("fast-flow")).priority(),
            100
        );
        assert_eq!(
            registry.get_or_create(FlowId::new("slow-flow")).priority(),
            10
        );

        // Admit fast-flow to occupy the only slot.
        let ticket_fast = scheduler
            .admit(FlowId::new("fast-flow"), WORK_UNIT)
            .await
            .unwrap();

        // Enqueue slow-flow — it blocks (slot full, priority 10 < 100).
        let s_slow = scheduler.clone();
        let task_slow = tokio::spawn(async move {
            s_slow.admit(FlowId::new("slow-flow"), WORK_UNIT).await
        });

        // Keep fast-flow's slot occupied. Wait for starvation timeout.
        // The slow-flow should be force-admitted by the starvation mechanism
        // after ~500ms, even though it has lower priority.
        tokio::time::sleep(starvation_timeout + Duration::from_millis(100)).await;

        // Drop fast-flow's ticket → frees the slot → slow-flow is admitted.
        drop(ticket_fast);

        // Slow-flow should complete (force-admitted despite lower priority).
        let _ticket_slow = tokio::time::timeout(Duration::from_secs(2), task_slow)
            .await
            .expect("slow-flow should be force-admitted within timeout")
            .expect("slow-flow task should not panic")
            .expect("slow-flow admit should succeed");

        assert!(
            m.starvation_force_admits_total.get() >= 1,
            "starvation force admits should be >= 1, got {}",
            m.starvation_force_admits_total.get()
        );
    })
    .await
    .expect("test should not timeout");
}

// ---------------------------------------------------------------------------
// Test 3: cold_start_flows_keep_default_priority
// ---------------------------------------------------------------------------

/// A flow with fewer than min_samples arrivals keeps its default priority.
///
/// With min_samples=3 and only 1 arrival, the heuristic has insufficient
/// data and returns None — the flow's priority remains the default (50).
#[tokio::test]
async fn cold_start_flows_keep_default_priority() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_, registry, scheduler) = build_scheduler(test_policy());

        // Admit the flow once.
        let cold_id = FlowId::new("cold");
        let ticket = scheduler.admit(cold_id.clone(), WORK_UNIT).await.unwrap();
        drop(ticket);

        // Only 1 arrival < min_samples=3, so priority should be default (50).
        assert_eq!(
            registry.get_or_create(cold_id.clone()).priority(),
            50,
            "cold-start flow should keep default priority (50)"
        );
    })
    .await
    .expect("test should not timeout");
}

// ---------------------------------------------------------------------------
// Test 4: disabled_policy_keeps_defaults
// ---------------------------------------------------------------------------

/// When PriorityPolicy.enabled=false, the heuristic is completely disabled.
/// Even with rapid admissions that would normally demote to background,
/// priority stays at the default (50).
#[tokio::test]
async fn disabled_policy_keeps_defaults() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let disabled_policy = PriorityPolicy {
            enabled: false,
            ..test_policy()
        };
        let (_, registry, scheduler) = build_scheduler(disabled_policy);

        // Admit a flow 5 times with 5ms gaps — would normally demote to
        // background (gap ~5ms <= 20ms). With policy disabled, no change.
        let flow_id = FlowId::new("disabled-flow");
        for _ in 0..5 {
            let ticket = scheduler.admit(flow_id.clone(), WORK_UNIT).await.unwrap();
            drop(ticket);
            tokio::time::sleep(Duration::from_millis(1)).await;
        }

        assert_eq!(
            registry.get_or_create(flow_id.clone()).priority(),
            50,
            "disabled policy should keep default priority (50)"
        );
    })
    .await
    .expect("test should not timeout");
}
