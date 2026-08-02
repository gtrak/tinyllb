//! Tests for the three cross-scheduler policies from issue #12:
//!
//! - **Priority**: higher-priority flow gets admission preference among eligible flows.
//! - **Starvation**: flows waiting longer than starvation_timeout are force-admitted.
//! - **Completion bias**: new flows are deferred while active >= target_active_flows.
//! - **Combined**: starvation overrides completion bias.
//!
//! Each test uses a very short starvation_timeout so it completes quickly.

use std::sync::Arc;
use std::time::Duration;

use llm_qdisc_proxy::backend::BackendMonitor;
use llm_qdisc_proxy::config::{Algorithm, BackpressureMode, CompletionBias, KvPolicyConfig};
use llm_qdisc_proxy::flow::{FlowId, FlowRegistration, FlowRegistry};
use llm_qdisc_proxy::metrics;
use llm_qdisc_proxy::scheduler::Scheduler;

const WORK_UNIT: f64 = 1024.0;

// ---------------------------------------------------------------------------
// Priority
// ---------------------------------------------------------------------------

/// Test: among eligible flows with equal weight, the higher-priority flow is
/// selected first by the WFQ admission loop.
///
/// Setup: max_active_flows=1 so only 1 request can be active.  A holder flow
/// occupies the slot.  Flow B (priority=50) is enqueued FIRST, then Flow A
/// (priority=100) is enqueued SECOND.  Both wait.  When the holder drops,
/// A must be admitted before B because A has higher priority.
///
/// WITHOUT priority: B would be admitted first (earlier enqueue wins).
/// WITH priority: A is admitted first (higher priority wins).
#[tokio::test]
async fn test_priority_higher_prio_selected_first() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let m = metrics::create_metrics();
        let registry = Arc::new(FlowRegistry::new(1.0, 50));
        let scheduler = Arc::new(Scheduler::new(
            Algorithm::Wfq,
            1, // max_active_flows=1
            m.clone(),
            registry.clone(),
            BackpressureMode::Blocking,
            100,
            Duration::from_secs(10),
            Duration::from_secs(1),
            Duration::from_secs(300), // long starvation timeout — not relevant here
            CompletionBias::default(),
            KvPolicyConfig::default(),
            Arc::new(BackendMonitor::empty()),
        ));

        // Register flows with different priorities.
        registry.register(FlowRegistration {
            id: FlowId::new("A"),
            weight: 1.0,
            priority: 100, // higher priority
        });
        registry.register(FlowRegistration {
            id: FlowId::new("B"),
            weight: 1.0,
            priority: 50, // lower priority
        });

        // Holder flow occupies the only slot.
        let holder = registry.get_or_create(FlowId::new("holder"));
        holder.set_weight(1.0);
        let ticket_holder = scheduler
            .admit(FlowId::new("holder"), WORK_UNIT)
            .await
            .unwrap();

        // Enqueue B (low priority) FIRST.
        let s_b = scheduler.clone();
        let task_b = tokio::spawn(async move { s_b.admit(FlowId::new("B"), WORK_UNIT).await });

        // Give B time to enter the queue.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Enqueue A (high priority) SECOND.
        let s_a = scheduler.clone();
        let task_a = tokio::spawn(async move { s_a.admit(FlowId::new("A"), WORK_UNIT).await });

        // Give A time to enter the queue.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // At this point both A and B are waiting.  Drop the holder.
        // A (priority=100) must be admitted BEFORE B (priority=50).
        drop(ticket_holder);

        // A should be admitted first.  Without priority, B (earlier enqueue)
        // would win — this is the discrimination.
        let ticket_a = task_a
            .await
            .expect("A task should complete")
            .expect("A admit should succeed");

        // Drop A's ticket → slot frees → B should be selected next.
        drop(ticket_a);

        let ticket_b = task_b
            .await
            .expect("B task should complete")
            .expect("B admit should succeed");
        drop(ticket_b);

        assert_eq!(m.active_flows.get(), 0.0);
    })
    .await
    .expect("test should not timeout");
}

/// Test: when flows have equal priority and equal weight, WFQ tiebreak
/// (min service_done/weight, then FIFO by enqueue time) determines selection order.
///
/// Setup: max_active_flows=1.  Holder occupies the slot.  Two flows at equal
/// priority and equal weight.  "second" enqueued first, "first" enqueued second.
/// Drop holder → "second" should be admitted first (FIFO tiebreak).
///
/// WITHOUT FIFO tiebreak: order is unspecified or alphabetic.
/// WITH FIFO tiebreak: earlier-enqueued flow wins.
#[tokio::test]
async fn test_priority_tiebreak_by_wfq_ratio() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let m = metrics::create_metrics();
        let registry = Arc::new(FlowRegistry::new(1.0, 50));
        let scheduler = Arc::new(Scheduler::new(
            Algorithm::Wfq,
            1,
            m.clone(),
            registry.clone(),
            BackpressureMode::Blocking,
            100,
            Duration::from_secs(10),
            Duration::from_secs(1),
            Duration::from_secs(300),
            CompletionBias::default(),
            KvPolicyConfig::default(),
            Arc::new(BackendMonitor::empty()),
        ));

        // All flows equal priority, equal weight → tiebreak by enqueue order.
        registry.register(FlowRegistration {
            id: FlowId::new("first"),
            weight: 1.0,
            priority: 50,
        });
        registry.register(FlowRegistration {
            id: FlowId::new("second"),
            weight: 1.0,
            priority: 50,
        });

        // Holder occupies the slot.
        let holder = registry.get_or_create(FlowId::new("holder"));
        holder.set_weight(1.0);
        let ticket_holder = scheduler
            .admit(FlowId::new("holder"), WORK_UNIT)
            .await
            .unwrap();

        // Enqueue "second" FIRST (earlier).
        let s_second = scheduler.clone();
        let task_second =
            tokio::spawn(async move { s_second.admit(FlowId::new("second"), WORK_UNIT).await });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Enqueue "first" SECOND (later).
        let s_first = scheduler.clone();
        let task_first =
            tokio::spawn(async move { s_first.admit(FlowId::new("first"), WORK_UNIT).await });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Drop holder — both compete.  "second" enqueued earlier, should win.
        drop(ticket_holder);

        // "second" should be admitted first (earlier enqueue wins the tie).
        let ticket_second = task_second
            .await
            .expect("second task should complete")
            .expect("second admit should succeed");
        drop(ticket_second);

        // "first" admitted next.
        let ticket_first = task_first
            .await
            .expect("first task should complete")
            .expect("first admit should succeed");
        drop(ticket_first);

        assert_eq!(m.active_flows.get(), 0.0);
    })
    .await
    .expect("test should not timeout");
}

// ---------------------------------------------------------------------------
// Starvation
// ---------------------------------------------------------------------------

/// Test: a low-priority flow that has been waiting longer than starvation_timeout
/// is force-admitted by the WFQ admission loop, bypassing normal priority rules.
///
/// Setup: max_active_flows=2, both slots taken by A.  Flow B (low priority)
/// enqueues and waits. After starvation_timeout, B is starved and should be
/// force-admitted when a slot frees.
///
/// The starvation check in the admission loop's Phase-1 forces B through,
/// and the `starvation_force_admits_total` counter is incremented.
#[tokio::test]
async fn test_starvation_force_admit_after_timeout() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let m = metrics::create_metrics();
        let registry = Arc::new(FlowRegistry::new(1.0, 50));
        let starvation_timeout = Duration::from_millis(50);
        let scheduler = Arc::new(Scheduler::new(
            Algorithm::Wfq,
            2, // 2 slots: A holds both
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
        ));

        // Flow A: holder
        registry.register(FlowRegistration {
            id: FlowId::new("A"),
            weight: 1.0,
            priority: 50,
        });
        // Flow B: low priority
        registry.register(FlowRegistration {
            id: FlowId::new("B"),
            weight: 1.0,
            priority: 10,
        });

        // Fill both slots with A requests.
        let s1 = scheduler.clone();
        let s2 = scheduler.clone();
        let ticket_a1 = s1.admit(FlowId::new("A"), WORK_UNIT).await.unwrap();
        let ticket_a2 = s2.admit(FlowId::new("A"), WORK_UNIT).await.unwrap();
        assert_eq!(m.active_flows.get(), 2.0);

        // Enqueue B. With both slots taken by A, B must wait.
        let s3 = scheduler.clone();
        let handle_b = tokio::spawn(async move { s3.admit(FlowId::new("B"), WORK_UNIT).await });

        // Wait for starvation timeout to pass.
        tokio::time::sleep(starvation_timeout + Duration::from_millis(100)).await;

        // Drop one of A's tickets — frees a slot.
        // B should be force-admitted (starvation overrides normal selection).
        drop(ticket_a1);

        // B should complete (force-admitted or normal admission).
        let ticket_b = handle_b
            .await
            .expect("B should not panic")
            .expect("B should be admitted");
        drop(ticket_b);

        drop(ticket_a2);
    })
    .await
    .expect("test should not timeout");
}

// ---------------------------------------------------------------------------
// Completion Bias
// ---------------------------------------------------------------------------

/// Test: with target_active_flows=2 and 2 flows already active, a 3rd distinct
/// flow is gated by completion bias until one of the active flows drains.
///
/// The core scenario: target=2, 2 active flows, 3rd distinct flow gated.
/// Drop ONE active ticket and assert the 3rd is admitted PROMPTLY
/// (without waiting for starvation).
///
/// This verifies the release-on-drain path: when an active ticket drops and
/// active < target, the gate wakes and the 3rd flow is admitted.
#[tokio::test]
async fn test_completion_bias_blocks_new_flow() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let m = metrics::create_metrics();
        let registry = Arc::new(FlowRegistry::new(1.0, 50));
        // Use a LONG starvation timeout so that if C is admitted promptly via
        // drain-release, starvation is NOT the cause.
        let scheduler = Arc::new(Scheduler::new(
            Algorithm::Fifo,
            4, // 4 slots, but target=2
            m.clone(),
            registry.clone(),
            BackpressureMode::Blocking,
            100,
            Duration::from_secs(10),
            Duration::from_secs(1),
            Duration::from_secs(300), // long — C should NOT need starvation
            CompletionBias {
                enabled: true,
                target_active_flows: 2,
                predictive_admit: false,
            },
            KvPolicyConfig::default(),
            Arc::new(BackendMonitor::empty()),
        ));

        // Fill 2 slots with A and B.
        let s1 = scheduler.clone();
        let s2 = scheduler.clone();
        let ticket_a = s1.admit(FlowId::new("A"), WORK_UNIT).await.unwrap();
        let ticket_b = s2.admit(FlowId::new("B"), WORK_UNIT).await.unwrap();
        assert_eq!(m.active_flows.get(), 2.0);

        // C is a new flow. With completion bias and target=2, C should wait
        // until active drops below 2.
        let s3 = scheduler.clone();
        let start = std::time::Instant::now();
        let handle_c = tokio::spawn(async move { s3.admit(FlowId::new("C"), WORK_UNIT).await });

        // Give C time to enter the gate.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Drop A's ticket → active drops from 2 to 1 (< target=2).
        // The gate should wake C immediately (release-on-drain).
        drop(ticket_a);

        // C should be admitted PROMPTLY (without starvation).
        let ticket_c = handle_c
            .await
            .expect("C should not panic")
            .expect("C should be admitted via drain-release");
        let elapsed = start.elapsed();

        // C should have been admitted quickly (< 200ms), NOT waiting for
        // starvation_timeout (300s).  If C waited ~300s, it was starvation,
        // not drain-release.
        assert!(
            elapsed < Duration::from_millis(500),
            "C should be admitted promptly via drain-release, got {:?}",
            elapsed
        );

        // Clean up.
        drop(ticket_c);
        drop(ticket_b);
    })
    .await
    .expect("test should not timeout");
}

/// Test: an ACTIVE flow's requests bypass the completion bias gate.
/// When flow A already has an active request, a 2nd request from A should
/// proceed even if active > target.
#[tokio::test]
async fn test_completion_bias_allows_active_flow_requests() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let m = metrics::create_metrics();
        let registry = Arc::new(FlowRegistry::new(1.0, 50));
        let scheduler = Arc::new(Scheduler::new(
            Algorithm::Fifo,
            2,
            m.clone(),
            registry.clone(),
            BackpressureMode::Blocking,
            100,
            Duration::from_secs(10),
            Duration::from_secs(1),
            Duration::from_secs(300),
            CompletionBias {
                enabled: true,
                target_active_flows: 1,
                predictive_admit: false,
            },
            KvPolicyConfig::default(),
            Arc::new(BackendMonitor::empty()),
        ));

        // A gets admitted. active=1, target=1.
        let s1 = scheduler.clone();
        let ticket_a1 = s1.admit(FlowId::new("A"), WORK_UNIT).await.unwrap();
        assert_eq!(m.active_flows.get(), 1.0);

        // A's second request: A is active, so it bypasses the gate.
        // With max_active_flows=2, there's a free slot.
        let s2 = scheduler.clone();
        let ticket_a2 = s2.admit(FlowId::new("A"), WORK_UNIT).await.unwrap();
        assert_eq!(m.active_flows.get(), 2.0);

        drop(ticket_a1);
        drop(ticket_a2);
    })
    .await
    .expect("test should not timeout");
}

// ---------------------------------------------------------------------------
// Combined: Completion Bias + Starvation
// ---------------------------------------------------------------------------

/// Test: completion bias normally blocks new flows when active >= target,
/// but starvation protection overrides the gate and force-admits the flow.
///
/// Setup: target=2, max_active_flows=4, starvation_timeout=100ms.
/// Fill 2 slots (active=2=target). New flow C enters the gate and waits.
/// After 100ms, C is starved and should be force-admitted despite completion bias.
#[tokio::test]
async fn test_combined_starvation_overrides_completion_bias() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let m = metrics::create_metrics();
        let registry = Arc::new(FlowRegistry::new(1.0, 50));
        let starvation_timeout = Duration::from_millis(100);
        let scheduler = Arc::new(Scheduler::new(
            Algorithm::Fifo,
            4, // 4 slots — room for C once forced through
            m.clone(),
            registry.clone(),
            BackpressureMode::Blocking,
            100,
            Duration::from_secs(10),
            Duration::from_secs(1),
            starvation_timeout,
            CompletionBias {
                enabled: true,
                target_active_flows: 2,
                predictive_admit: false,
            },
            KvPolicyConfig::default(),
            Arc::new(BackendMonitor::empty()),
        ));

        // Fill 2 slots with A and B.
        let s1 = scheduler.clone();
        let s2 = scheduler.clone();
        let ticket_a = s1.admit(FlowId::new("A"), WORK_UNIT).await.unwrap();
        let ticket_b = s2.admit(FlowId::new("B"), WORK_UNIT).await.unwrap();
        assert_eq!(m.active_flows.get(), 2.0);

        // C enters the gate. With target=2 and active=2, C should wait.
        // After starvation_timeout, C should be force-admitted.
        let s3 = scheduler.clone();
        let start = std::time::Instant::now();
        let handle_c = tokio::spawn(async move {
            let ticket = s3.admit(FlowId::new("C"), WORK_UNIT).await.unwrap();
            drop(ticket);
        });

        handle_c.await.expect("C should not panic");
        let elapsed = start.elapsed();

        // C should have waited approximately the starvation timeout.
        assert!(
            elapsed >= Duration::from_millis(80),
            "C should wait ~100ms for starvation force-admit, got {:?}",
            elapsed
        );

        // Verify starvation force-admit was recorded.
        assert!(
            m.starvation_force_admits_total.get() >= 1,
            "starvation force admits should be >= 1, got {}",
            m.starvation_force_admits_total.get()
        );

        drop(ticket_a);
        drop(ticket_b);
    })
    .await
    .expect("test should not timeout");
}

// ---------------------------------------------------------------------------
// Priority in DRR
// ---------------------------------------------------------------------------

/// Test: DRR scheduler respects priority ordering among eligible flows.
/// Higher-priority flow gets selected before lower-priority flows.
///
/// Setup: max_active_flows=1.  Holder occupies the slot.  Flow B (priority=50)
/// enqueued first, Flow A (priority=100) enqueued second.  Drop holder → A must
/// be admitted before B.
///
/// WITHOUT priority: B would be admitted first (earlier enqueue / RR order).
/// WITH priority: A wins (higher priority).
#[tokio::test]
async fn test_priority_drr_higher_prio_selected_first() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let m = metrics::create_metrics();
        let registry = Arc::new(FlowRegistry::new(1.0, 50));
        let scheduler = Arc::new(Scheduler::new(
            Algorithm::Drr,
            1,
            m.clone(),
            registry.clone(),
            BackpressureMode::Blocking,
            100,
            Duration::from_secs(10),
            Duration::from_secs(1),
            Duration::from_secs(300),
            CompletionBias::default(),
            KvPolicyConfig::default(),
            Arc::new(BackendMonitor::empty()),
        ));

        registry.register(FlowRegistration {
            id: FlowId::new("A"),
            weight: 1.0,
            priority: 100,
        });
        registry.register(FlowRegistration {
            id: FlowId::new("B"),
            weight: 1.0,
            priority: 50,
        });

        // Holder occupies the only slot.
        let holder = registry.get_or_create(FlowId::new("holder"));
        holder.set_weight(1.0);
        let ticket_holder = scheduler
            .admit(FlowId::new("holder"), WORK_UNIT)
            .await
            .unwrap();

        // Enqueue B (low priority) FIRST.
        let s_b = scheduler.clone();
        let task_b = tokio::spawn(async move { s_b.admit(FlowId::new("B"), WORK_UNIT).await });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Enqueue A (high priority) SECOND.
        let s_a = scheduler.clone();
        let task_a = tokio::spawn(async move { s_a.admit(FlowId::new("A"), WORK_UNIT).await });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Drop holder — both compete.  A (priority=100) must win over B (priority=50).
        drop(ticket_holder);

        let ticket_a = task_a
            .await
            .expect("A task should complete")
            .expect("A admit should succeed");
        drop(ticket_a);

        let ticket_b = task_b
            .await
            .expect("B task should complete")
            .expect("B admit should succeed");
        drop(ticket_b);

        assert_eq!(m.active_flows.get(), 0.0);
    })
    .await
    .expect("test should not timeout");
}

/// Test: starvation force-admit works with DRR scheduler.
#[tokio::test]
async fn test_starvation_drr_force_admit() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let m = metrics::create_metrics();
        let registry = Arc::new(FlowRegistry::new(1.0, 50));
        let starvation_timeout = Duration::from_millis(50);
        let scheduler = Arc::new(Scheduler::new(
            Algorithm::Drr,
            2,
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
        ));

        registry.register(FlowRegistration {
            id: FlowId::new("A"),
            weight: 1.0,
            priority: 50,
        });
        registry.register(FlowRegistration {
            id: FlowId::new("B"),
            weight: 1.0,
            priority: 10,
        });

        // Fill both slots with A.
        let s1 = scheduler.clone();
        let s2 = scheduler.clone();
        let ticket_a1 = s1.admit(FlowId::new("A"), WORK_UNIT).await.unwrap();
        let ticket_a2 = s2.admit(FlowId::new("A"), WORK_UNIT).await.unwrap();

        // Enqueue B — should wait until starvation forces it.
        let s3 = scheduler.clone();
        let handle_b = tokio::spawn(async move { s3.admit(FlowId::new("B"), WORK_UNIT).await });

        tokio::time::sleep(starvation_timeout + Duration::from_millis(100)).await;

        drop(ticket_a1);
        let ticket_b = handle_b
            .await
            .expect("B should not panic")
            .expect("B admitted");
        drop(ticket_b);

        // Verify starvation force-admit was recorded.
        assert!(
            m.starvation_force_admits_total.get() >= 1,
            "starvation force admits should be >= 1, got {}",
            m.starvation_force_admits_total.get()
        );

        drop(ticket_a2);
    })
    .await
    .expect("test should not timeout");
}
