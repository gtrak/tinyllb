//! End-to-end tests for the turn-boundary priority state machine (Plan 006, Task 05).
//!
//! These tests verify that the cadence state machine correctly classifies flows
//! via `Scheduler::admit_with_turn_boundary` and that higher-priority flows win
//! scheduling contention over lower-priority flows.
//!
//! IMPORTANT: These tests use real wall-clock time via `tokio::time::sleep`
//! WITHOUT `tokio::time::pause()`. The cadence state machine calls
//! `std::time::Instant::now()` which is not affected by tokio's fake clock.
//! Real sleeps advance the wall clock that Instant observes.

use std::sync::Arc;
use std::time::Duration;

use tinyllb::backend::BackendMonitor;
use tinyllb::config::{
    Algorithm, BackpressureMode, CompletionBias, KvPolicyConfig, Priorities, PriorityPolicy,
};
use tinyllb::flow::FlowRegistry;
use tinyllb::metrics::{self, Metrics};
use tinyllb::scheduler::Scheduler;
use tinyllb::flow::FlowId;

const WORK_UNIT: f64 = 1024.0;

/// Test-specific priority policy with small thresholds for fast testing.
///
/// - idle_gap_threshold: 50ms (gaps >= 50ms at a turn boundary -> idle chunk)
/// - agentic_suspected_threshold: 5 (5 continuous tool arrivals -> AgenticSuspected)
/// - agentic_confirmed_threshold: 12 (12 continuous tool arrivals -> AgenticConfirmed)
fn test_policy() -> PriorityPolicy {
    PriorityPolicy {
        enabled: true,
        idle_gap_threshold: Duration::from_millis(50),
        agentic_suspected_threshold: 5,
        agentic_confirmed_threshold: 12,
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
// Test 1: interactive_flow_wins_over_agentic
// ---------------------------------------------------------------------------

/// End-to-end test: a flow that becomes Interactive (priority 100) wins
/// scheduling contention over a flow that is AgenticConfirmed (priority 10).
///
/// Exercises the full pipeline:
/// admit_with_turn_boundary -> record_arrival -> state machine -> classify_and_apply
/// -> priority lookup in DRR -> slot assignment -> contention resolution.
#[tokio::test]
async fn interactive_flow_wins_over_agentic() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (m, registry, scheduler) = build_scheduler(test_policy());

        // ── Build cadence for tool-flow: 12 rapid tool admits -> AgenticConfirmed (10) ──
        let tool_id = FlowId::new("tool-flow");
        for _ in 0..12 {
            let ticket = scheduler
                .admit_with_turn_boundary(tool_id.clone(), WORK_UNIT, false)
                .await
                .unwrap();
            drop(ticket);
        }
        assert_eq!(
            registry.get_or_create(tool_id.clone()).priority(),
            10,
            "tool-flow should be AgenticConfirmed (10)"
        );

        // ── Build cadence for user-flow: 1 user admit with >50ms gap -> Interactive (100) ──
        // Sleep 60ms to create a gap > idle_gap_threshold (50ms).
        tokio::time::sleep(Duration::from_millis(60)).await;
        let user_id = FlowId::new("user-flow");
        let ticket = scheduler
            .admit_with_turn_boundary(user_id.clone(), WORK_UNIT, true)
            .await
            .unwrap();
        drop(ticket);
        assert_eq!(
            registry.get_or_create(user_id.clone()).priority(),
            100,
            "user-flow should be Interactive (100)"
        );

        // ── Contention round ──
        // Holder occupies the only slot.
        let ticket_holder = scheduler
            .admit(FlowId::new("holder"), WORK_UNIT)
            .await
            .unwrap();

        // Enqueue tool-flow (priority 10) in background task.
        let s_tool = scheduler.clone();
        let task_tool = tokio::spawn(async move {
            s_tool.admit_with_turn_boundary(FlowId::new("tool-flow"), WORK_UNIT, false).await
        });

        // Give tool-flow time to enter the queue.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Enqueue user-flow (priority 100) in background task.
        let s_user = scheduler.clone();
        let task_user = tokio::spawn(async move {
            s_user.admit_with_turn_boundary(FlowId::new("user-flow"), WORK_UNIT, true).await
        });

        // Give user-flow time to enter the queue.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Drop holder -> slot frees. User-flow (priority 100) should be admitted first.
        drop(ticket_holder);

        // Await user-flow first — it should complete within the timeout.
        let ticket_user = tokio::time::timeout(Duration::from_secs(2), task_user)
            .await
            .expect("user-flow admit should not timeout")
            .expect("user-flow task should not panic")
            .expect("user-flow admit should succeed");

        // Drop user-flow's ticket -> tool-flow gets the slot.
        drop(ticket_user);

        let ticket_tool = tokio::time::timeout(Duration::from_secs(2), task_tool)
            .await
            .expect("tool-flow admit should not timeout")
            .expect("tool-flow task should not panic")
            .expect("tool-flow admit should succeed");

        drop(ticket_tool);
        assert_eq!(m.active_flows.get(), 0.0);
    })
    .await
    .expect("test should not timeout");
}

// ---------------------------------------------------------------------------
// Test 2: reactive_promotion
// ---------------------------------------------------------------------------

/// A flow that is AgenticConfirmed (priority 10) can be reactively promoted
/// to Interactive (priority 100) by a turn-boundary idle. After promotion,
/// it wins over a fresh AgenticConfirmed flow.
#[tokio::test]
async fn reactive_promotion() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (m, registry, scheduler) = build_scheduler(test_policy());

        let flow_id = FlowId::new("reactive-flow");

        // ── Step 1: 12 tool admits -> AgenticConfirmed (priority 10) ──
        for _ in 0..12 {
            let ticket = scheduler
                .admit_with_turn_boundary(flow_id.clone(), WORK_UNIT, false)
                .await
                .unwrap();
            drop(ticket);
        }
        assert_eq!(
            registry.get_or_create(flow_id.clone()).priority(),
            10,
            "reactive-flow should be AgenticConfirmed (10)"
        );

        // ── Step 2: User admit with >50ms gap -> Interactive (priority 100) ──
        tokio::time::sleep(Duration::from_millis(60)).await;
        let ticket = scheduler
            .admit_with_turn_boundary(flow_id.clone(), WORK_UNIT, true)
            .await
            .unwrap();
        drop(ticket);
        assert_eq!(
            registry.get_or_create(flow_id.clone()).priority(),
            100,
            "reactive-flow should be promoted to Interactive (100)"
        );

        // ── Step 3: Build a competing AgenticConfirmed flow ──
        let comp_id = FlowId::new("competitor-flow");
        for _ in 0..12 {
            let ticket = scheduler
                .admit_with_turn_boundary(comp_id.clone(), WORK_UNIT, false)
                .await
                .unwrap();
            drop(ticket);
        }
        assert_eq!(
            registry.get_or_create(comp_id.clone()).priority(),
            10,
            "competitor should be AgenticConfirmed (10)"
        );

        // ── Step 4: Contention — promoted flow should win over competitor ──
        let ticket_holder = scheduler
            .admit(FlowId::new("holder"), WORK_UNIT)
            .await
            .unwrap();

        let s_comp = scheduler.clone();
        let task_comp = tokio::spawn(async move {
            s_comp.admit_with_turn_boundary(FlowId::new("competitor-flow"), WORK_UNIT, false).await
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        let s_reactive = scheduler.clone();
        let task_reactive = tokio::spawn(async move {
            s_reactive
                .admit_with_turn_boundary(FlowId::new("reactive-flow"), WORK_UNIT, true)
                .await
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        drop(ticket_holder);

        // Promoted flow (100) should win over competitor (10).
        let ticket_reactive = tokio::time::timeout(Duration::from_secs(2), task_reactive)
            .await
            .expect("reactive admit should not timeout")
            .expect("reactive task should not panic")
            .expect("reactive admit should succeed");

        drop(ticket_reactive);

        let ticket_comp = tokio::time::timeout(Duration::from_secs(2), task_comp)
            .await
            .expect("competitor admit should not timeout")
            .expect("competitor task should not panic")
            .expect("competitor admit should succeed");

        drop(ticket_comp);
        assert_eq!(m.active_flows.get(), 0.0);
    })
    .await
    .expect("test should not timeout");
}

// ---------------------------------------------------------------------------
// Test 3: cold_start_optimistic
// ---------------------------------------------------------------------------

/// A brand-new flow's first admit gets priority 100 (Cold) and wins over
/// an AgenticConfirmed flow (priority 10).
#[tokio::test]
async fn cold_start_optimistic() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (m, registry, scheduler) = build_scheduler(test_policy());

        // ── Build AgenticConfirmed competitor ──
        let comp_id = FlowId::new("agentic-comp");
        for _ in 0..12 {
            let ticket = scheduler
                .admit_with_turn_boundary(comp_id.clone(), WORK_UNIT, false)
                .await
                .unwrap();
            drop(ticket);
        }
        assert_eq!(
            registry.get_or_create(comp_id.clone()).priority(),
            10,
            "competitor should be AgenticConfirmed (10)"
        );

        // ── Cold-start flow: first admit -> Cold (priority 100) ──
        // New flows start at Cold, which maps to interactive (100).
        let cold_id = FlowId::new("cold-start");
        let ticket = scheduler
            .admit_with_turn_boundary(cold_id.clone(), WORK_UNIT, true)
            .await
            .unwrap();
        drop(ticket);
        assert_eq!(
            registry.get_or_create(cold_id.clone()).priority(),
            100,
            "cold-start flow should be Cold/Interactive (100)"
        );

        // ── Contention: cold-start (100) should beat agentic (10) ──
        let ticket_holder = scheduler
            .admit(FlowId::new("holder"), WORK_UNIT)
            .await
            .unwrap();

        let s_comp = scheduler.clone();
        let task_comp = tokio::spawn(async move {
            s_comp.admit_with_turn_boundary(FlowId::new("agentic-comp"), WORK_UNIT, false).await
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        let s_cold = scheduler.clone();
        let task_cold = tokio::spawn(async move {
            s_cold.admit_with_turn_boundary(FlowId::new("cold-start"), WORK_UNIT, true).await
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        drop(ticket_holder);

        // Cold-start (100) should win over agentic (10).
        let ticket_cold = tokio::time::timeout(Duration::from_secs(2), task_cold)
            .await
            .expect("cold-start admit should not timeout")
            .expect("cold-start task should not panic")
            .expect("cold-start admit should succeed");

        drop(ticket_cold);

        let ticket_comp = tokio::time::timeout(Duration::from_secs(2), task_comp)
            .await
            .expect("agentic admit should not timeout")
            .expect("agentic task should not panic")
            .expect("agentic admit should succeed");

        drop(ticket_comp);
        assert_eq!(m.active_flows.get(), 0.0);
    })
    .await
    .expect("test should not timeout");
}
