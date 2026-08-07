//! KV-Cache-Aware Admission tests (Issue 15).
//!
//! Tests drive controllable KV cache snapshots through the full proxy
//! stack to verify accept/delay/reject decisions.

use std::sync::Arc;
use std::time::Duration;

use tinyllb::backend::{BackendMonitor, BackendSnapshot};
use tinyllb::config::{Algorithm, BackpressureMode, KvPolicyConfig, Priorities, PriorityPolicy};
use tinyllb::flow::{FlowId, FlowRegistry};
use tinyllb::metrics;
use tinyllb::scheduler::Scheduler;

// ---------------------------------------------------------------------------
// Helper: build scheduler with KV policy
// ---------------------------------------------------------------------------

fn build_scheduler(kv_config: KvPolicyConfig, monitor: Arc<BackendMonitor>) -> Arc<Scheduler> {
    build_scheduler_with_mode(kv_config, monitor, BackpressureMode::Blocking)
}

fn build_scheduler_with_mode(
    kv_config: KvPolicyConfig,
    monitor: Arc<BackendMonitor>,
    mode: BackpressureMode,
) -> Arc<Scheduler> {
    let m = metrics::create_metrics();
    let registry = Arc::new(FlowRegistry::new(1.0, 50));
    let scheduler = Scheduler::new(
        Algorithm::Fifo,
        4,
        m.clone(),
        registry,
        mode,
        100,
        Duration::from_secs(10),
        Duration::from_secs(1),
        Duration::from_secs(300),
        Default::default(), // completion_bias
        kv_config,
        monitor,
        PriorityPolicy::default(),
        Priorities::default(),
        tinyllb::config::KvBias::default(),
    );
    Arc::new(scheduler)
}

fn enabled_kv_policy() -> KvPolicyConfig {
    KvPolicyConfig {
        enabled: true,
        reject_threshold: 0.95,
        delay_threshold: 0.80,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Below delay_threshold → Accept immediately.
#[tokio::test]
async fn kv_admission_accept_below_threshold() {
    let (tx, rx) = tokio::sync::watch::channel(BackendSnapshot {
        kv_usage: 0.3,
        kv_free: 0.7,
        preemptions: 0,
    });
    let monitor = Arc::new(BackendMonitor::from_receiver(rx));
    let _tx = tx; // keep sender alive

    let scheduler = build_scheduler(enabled_kv_policy(), monitor);

    let start = std::time::Instant::now();
    let ticket = tokio::time::timeout(
        Duration::from_secs(1),
        scheduler.admit(FlowId::new("test"), 1024.0),
    )
    .await
    .expect("should not timeout")
    .expect("admit should succeed");
    let elapsed = start.elapsed();
    drop(ticket);

    assert!(
        elapsed < Duration::from_millis(100),
        "Accept should be instant, took {:?}",
        elapsed
    );
}

/// Between delay and reject thresholds → Delay until usage drops.
#[tokio::test]
async fn kv_admission_delay_until_drop() {
    let (tx, rx) = tokio::sync::watch::channel(BackendSnapshot {
        kv_usage: 0.85,
        kv_free: 0.15,
        preemptions: 0,
    });
    let monitor = Arc::new(BackendMonitor::from_receiver(rx));

    let scheduler = build_scheduler(enabled_kv_policy(), monitor);

    // Start the admit in a task — it will block on delay.
    let sched_clone = scheduler.clone();
    let admit_task =
        tokio::spawn(async move { sched_clone.admit(FlowId::new("test"), 1024.0).await });

    // Wait a bit for the task to enter the delay loop.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Drop usage below delay_threshold — the task should unblock.
    let _ = tx.send(BackendSnapshot {
        kv_usage: 0.5,
        kv_free: 0.5,
        preemptions: 0,
    });

    let ticket = tokio::time::timeout(Duration::from_secs(5), admit_task)
        .await
        .expect("admit should unblock after drop")
        .expect("inner admit should succeed");
    drop(ticket);
}

/// Above reject_threshold → Reject with BackpressureRejected.
#[tokio::test]
async fn kv_admission_reject_above_threshold() {
    let (tx, rx) = tokio::sync::watch::channel(BackendSnapshot {
        kv_usage: 0.96,
        kv_free: 0.04,
        preemptions: 5,
    });
    let monitor = Arc::new(BackendMonitor::from_receiver(rx));
    let _tx = tx;

    let scheduler = build_scheduler(enabled_kv_policy(), monitor);

    // Should be rejected immediately.
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        scheduler.admit(FlowId::new("test"), 1024.0),
    )
    .await
    .expect("should not timeout");

    match result {
        Ok(_) => panic!("admit should be rejected at 0.96 usage"),
        Err(rejected) => {
            // Retry-After should be ~5s + excess scaling.
            assert!(
                rejected.retry_after >= Duration::from_secs(5),
                "Retry-After should be >= 5s, got {:?}",
                rejected.retry_after
            );
        }
    }
}

/// KV policy disabled → always accept regardless of usage.
#[tokio::test]
async fn kv_admission_disabled_always_accepts() {
    let (tx, rx) = tokio::sync::watch::channel(BackendSnapshot {
        kv_usage: 0.99,
        kv_free: 0.01,
        preemptions: 100,
    });
    let monitor = Arc::new(BackendMonitor::from_receiver(rx));
    let _tx = tx;

    let kv_config = KvPolicyConfig {
        enabled: false,
        reject_threshold: 0.95,
        delay_threshold: 0.80,
    };

    let scheduler = build_scheduler(kv_config, monitor);

    let ticket = scheduler
        .admit(FlowId::new("test"), 1024.0)
        .await
        .expect("disabled policy should accept");
    drop(ticket);
}

/// Empty monitor (default snapshot: 0.0 usage) → accept.
#[tokio::test]
async fn kv_admission_empty_monitor_accepts() {
    let monitor = Arc::new(BackendMonitor::empty());

    let scheduler = build_scheduler(enabled_kv_policy(), monitor);

    let ticket = scheduler
        .admit(FlowId::new("test"), 1024.0)
        .await
        .expect("empty monitor (0.0 usage) should accept");
    drop(ticket);
}

/// Verify decision counter increments correctly.
#[tokio::test]
async fn kv_admission_decision_counter_increments() {
    let (tx, rx) = tokio::sync::watch::channel(BackendSnapshot {
        kv_usage: 0.3,
        kv_free: 0.7,
        preemptions: 0,
    });
    let monitor = Arc::new(BackendMonitor::from_receiver(rx));

    let m = metrics::create_metrics();
    let registry = Arc::new(FlowRegistry::new(1.0, 50));
    let scheduler = Arc::new(Scheduler::new(
        Algorithm::Fifo,
        4,
        m.clone(),
        registry,
        BackpressureMode::Blocking,
        100,
        Duration::from_secs(10),
        Duration::from_secs(1),
        Duration::from_secs(300),
        Default::default(),
        enabled_kv_policy(),
        monitor,
        PriorityPolicy::default(),
        Priorities::default(),
        tinyllb::config::KvBias::default(),
    ));

    // 1 accept
    let ticket = scheduler
        .admit(FlowId::new("a"), 1024.0)
        .await
        .expect("admit 1");
    drop(ticket);

    // Update to reject zone.
    let _ = tx.send(BackendSnapshot {
        kv_usage: 0.96,
        kv_free: 0.04,
        preemptions: 0,
    });

    // 1 reject
    let result = scheduler.admit(FlowId::new("b"), 1024.0).await;
    assert!(result.is_err(), "should reject");

    // Update to accept zone.
    let _ = tx.send(BackendSnapshot {
        kv_usage: 0.0,
        kv_free: 1.0,
        preemptions: 0,
    });

    // 1 more accept
    let ticket = scheduler
        .admit(FlowId::new("c"), 1024.0)
        .await
        .expect("admit 2");
    drop(ticket);

    // Verify counters.
    let accept_count = m
        .kv_admission_decisions_total
        .with_label_values(&["accept"])
        .get();
    let reject_count = m
        .kv_admission_decisions_total
        .with_label_values(&["reject"])
        .get();

    assert_eq!(accept_count, 2.0, "should have 2 accept decisions");
    assert_eq!(reject_count, 1.0, "should have 1 reject decision");
}

/// Delay transitions to accept when usage drops during wait.
#[tokio::test]
async fn kv_admission_delay_to_accept_transition() {
    let (tx, rx) = tokio::sync::watch::channel(BackendSnapshot {
        kv_usage: 0.0,
        kv_free: 1.0,
        preemptions: 0,
    });
    let monitor = Arc::new(BackendMonitor::from_receiver(rx));

    let m = metrics::create_metrics();
    let registry = Arc::new(FlowRegistry::new(1.0, 50));
    let scheduler = Arc::new(Scheduler::new(
        Algorithm::Fifo,
        4,
        m.clone(),
        registry,
        BackpressureMode::Blocking,
        100,
        Duration::from_secs(10),
        Duration::from_secs(1),
        Duration::from_secs(300),
        Default::default(),
        enabled_kv_policy(),
        monitor,
        PriorityPolicy::default(),
        Priorities::default(),
        tinyllb::config::KvBias::default(),
    ));

    // First request: accept immediately.
    let ticket = scheduler
        .admit(FlowId::new("a"), 1024.0)
        .await
        .expect("initial admit");
    drop(ticket);

    // Spike usage to reject zone, then back to accept.
    let _ = tx.send(BackendSnapshot {
        kv_usage: 0.85,
        kv_free: 0.15,
        preemptions: 0,
    });

    // Start a request that will delay.
    let sched_clone = scheduler.clone();
    let admit_task = tokio::spawn(async move { sched_clone.admit(FlowId::new("b"), 1024.0).await });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Drop to accept zone.
    let _ = tx.send(BackendSnapshot {
        kv_usage: 0.3,
        kv_free: 0.7,
        preemptions: 0,
    });

    let ticket = tokio::time::timeout(Duration::from_secs(5), admit_task)
        .await
        .expect("should unblock")
        .expect("should succeed after drop");
    drop(ticket);

    // Check counters:
    // - admit a: KV accept → accept=1
    // - admit b: KV delay → delay=1 (wakes when dropped, no extra accept count)
    let accept_count = m
        .kv_admission_decisions_total
        .with_label_values(&["accept"])
        .get();
    let delay_count = m
        .kv_admission_decisions_total
        .with_label_values(&["delay"])
        .get();

    assert_eq!(
        accept_count, 1.0,
        "should have 1 accept decision (from admit a)"
    );
    assert_eq!(
        delay_count, 1.0,
        "should have 1 delay decision (from admit b)"
    );
}

// ---------------------------------------------------------------------------
// NEW tests (review-flagged fixes)
// ---------------------------------------------------------------------------

/// Hybrid mode + delay band + max_wait elapsed → request rejected with 429
/// (not hung indefinitely). This is the fix for the unbounded delay wait
/// defect.
#[tokio::test]
async fn kv_admission_hybrid_delay_timeout_rejected() {
    // Usage pinned in delay band (0.85 > 0.80 delay_threshold).
    let (tx, _rx) = tokio::sync::watch::channel(BackendSnapshot {
        kv_usage: 0.85,
        kv_free: 0.15,
        preemptions: 0,
    });
    let monitor = Arc::new(BackendMonitor::from_receiver(_rx));
    let _tx = tx; // keep sender alive — usage never drops, simulating stuck KV

    // Build with Hybrid mode and a SHORT max_wait for fast test execution.
    let m = metrics::create_metrics();
    let registry = Arc::new(FlowRegistry::new(1.0, 50));
    let scheduler = Arc::new(Scheduler::new(
        Algorithm::Fifo,
        4,
        m.clone(),
        registry,
        BackpressureMode::Hybrid,
        100,
        Duration::from_millis(200), // short max_wait
        Duration::from_secs(1),
        Duration::from_secs(300),
        Default::default(),
        enabled_kv_policy(),
        monitor,
        PriorityPolicy::default(),
        Priorities::default(),
        tinyllb::config::KvBias::default(),
    ));

    // The request should be rejected after max_wait (200ms), not hung.
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        scheduler.admit(FlowId::new("test"), 1024.0),
    )
    .await
    .expect("should not exceed outer 5s timeout");

    match result {
        Ok(_) => panic!("should be rejected in hybrid mode when delay wait times out"),
        Err(rejected) => {
            // Should be a BackpressureRejected with some retry_after.
            assert!(
                !rejected.retry_after.is_zero(),
                "should have a non-zero Retry-After"
            );
        }
    }
}

/// Delayed requests should be visible in queue_depth and queue_snapshot.
/// This is the fix for the "delayed requests invisible to the queue" defect.
#[tokio::test]
async fn kv_admission_delayed_visible_in_queue_depth() {
    // Usage in delay band (0.85 > 0.80 delay_threshold).
    let (tx, rx) = tokio::sync::watch::channel(BackendSnapshot {
        kv_usage: 0.85,
        kv_free: 0.15,
        preemptions: 0,
    });
    let monitor = Arc::new(BackendMonitor::from_receiver(rx));

    // Build with Hybrid mode and short max_wait so the test doesn't hang.
    let m = metrics::create_metrics();
    let registry = Arc::new(FlowRegistry::new(1.0, 50));
    let scheduler = Arc::new(Scheduler::new(
        Algorithm::Fifo,
        4,
        m.clone(),
        registry,
        BackpressureMode::Hybrid,
        100,
        Duration::from_secs(30), // long enough that delay is still active
        Duration::from_secs(1),
        Duration::from_secs(300),
        Default::default(),
        enabled_kv_policy(),
        monitor,
        PriorityPolicy::default(),
        Priorities::default(),
        tinyllb::config::KvBias::default(),
    ));

    // Queue depth should be 0 before any admit.
    assert_eq!(
        scheduler.queue_depth(),
        0,
        "initial queue depth should be 0"
    );

    // Spawn an admit that will enter the delay wait.
    let sched_clone = scheduler.clone();
    let admit_task =
        tokio::spawn(async move { sched_clone.admit(FlowId::new("delayed-flow"), 1024.0).await });

    // Wait for the delay guard to be active.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Queue depth should now include the delayed request.
    let depth = scheduler.queue_depth();
    assert!(
        depth >= 1,
        "queue_depth should include delayed request, got {}",
        depth
    );

    // Queue snapshot should also show increased waiting count.
    let snapshot = scheduler.queue_snapshot();
    assert!(
        snapshot.waiting >= 1,
        "queue_snapshot waiting should include delayed request, got {}",
        snapshot.waiting
    );

    // Drop usage so the delayed request proceeds (avoid hanging the test).
    let _ = tx.send(BackendSnapshot {
        kv_usage: 0.3,
        kv_free: 0.7,
        preemptions: 0,
    });

    // Wait for the task to complete.
    let _ = tokio::time::timeout(Duration::from_secs(5), admit_task)
        .await
        .expect("delayed request should proceed after usage drops");
}
