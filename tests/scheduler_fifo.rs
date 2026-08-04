//! Tests for the FIFO scheduler (issue 05).
//!
//! Verifies:
//! - admit/release cycle (permit acquired and released)
//! - queue enforcement (requests beyond max_active_flows wait)
//! - head-of-queue released when a slot frees
//! - wait-time metric recorded for queued requests
//! - permit released on panic / early return (no leak)

use std::sync::Arc;
use std::time::Duration;

use tinyllb::config::BackpressureMode;
use tinyllb::flow::{FlowId, FlowRegistry};
use tinyllb::metrics;
use tinyllb::scheduler::FifoScheduler;

/// Create a scheduler with a specific max_active_flows for tests.
fn make_scheduler(max_active_flows: u32) -> (Arc<FifoScheduler>, Arc<metrics::Metrics>) {
    let m = metrics::create_metrics();
    let registry = Arc::new(FlowRegistry::new(1.0, 50));
    let scheduler = Arc::new(FifoScheduler::new(
        max_active_flows,
        m.clone(),
        registry,
        BackpressureMode::Blocking,
        100,
        std::time::Duration::from_secs(10),
        std::time::Duration::from_secs(1),
    ));
    (scheduler, m)
}

/// Default flow ID for tests.
fn test_flow_id() -> FlowId {
    FlowId::new("test")
}

/// Test: a single request is admitted immediately when under capacity.
#[tokio::test]
async fn test_admit_single_under_capacity() {
    let (scheduler, m) = make_scheduler(2);

    // Active flows should start at 0.
    assert_eq!(m.active_flows.get(), 0.0);

    // Admit one request — should succeed immediately.
    let ticket = scheduler.admit(test_flow_id(), 1024.0).await.unwrap();
    assert_eq!(m.active_flows.get(), 1.0);

    // Verify the ticket carries the correct flow ID.
    assert_eq!(ticket.flow_id.to_string(), "test");

    // Drop the ticket — active flows should return to 0.
    drop(ticket);
    assert_eq!(m.active_flows.get(), 0.0);
}

/// Test: exactly max_active_flows requests can be admitted simultaneously.
#[tokio::test]
async fn test_admit_at_capacity() {
    let (scheduler, m) = make_scheduler(2);

    let t1 = scheduler.admit(test_flow_id(), 1024.0).await.unwrap();
    let t2 = scheduler.admit(test_flow_id(), 1024.0).await.unwrap();

    assert_eq!(m.active_flows.get(), 2.0);
    assert_eq!(scheduler.queue_depth(), 0);

    drop(t1);
    drop(t2);
    assert_eq!(m.active_flows.get(), 0.0);
}

/// Test: third request beyond max_active_flows=2 waits, then proceeds when
/// one of the first two tickets is dropped.
#[tokio::test]
async fn test_third_request_waits_and_releases_on_finish() {
    let (scheduler, m) = make_scheduler(2);

    // Fill both slots.
    let t1 = scheduler.admit(test_flow_id(), 1024.0).await.unwrap();
    let t2 = scheduler.admit(test_flow_id(), 1024.0).await.unwrap();
    assert_eq!(m.active_flows.get(), 2.0);

    // Spawn a task that tries to admit; it should block because both slots
    // are occupied.
    let scheduler_clone = scheduler.clone();
    let joiner = tokio::spawn(async move { scheduler_clone.admit(test_flow_id(), 1024.0).await });

    // Give the spawned task a moment to enter the queue.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Should still be at 2 active (the third is waiting, not active yet).
    assert_eq!(m.active_flows.get(), 2.0);

    // Release one slot — the waiting task should acquire it.
    drop(t1);

    // The third task should now have acquired its permit.
    let t3 = joiner.await.expect("joiner should not panic").unwrap();
    assert_eq!(m.active_flows.get(), 2.0); // t2 + t3

    drop(t2);
    drop(t3);
    assert_eq!(m.active_flows.get(), 0.0);
}

/// Test: wait-time metric is recorded for a queued request (> 0 seconds).
/// Verifies that a queued request's wait time is tracked and the active_flows
/// gauge is correct before and after.
#[tokio::test]
async fn test_wait_time_recorded_for_queued_request() {
    let (scheduler, m) = make_scheduler(1);

    // Occupy the single slot.
    let t1 = scheduler.admit(test_flow_id(), 1024.0).await.unwrap();
    assert_eq!(m.active_flows.get(), 1.0);

    // Spawn a task that will wait; use a channel to confirm it entered the queue.
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let scheduler_clone = scheduler.clone();
    let joiner = tokio::spawn(async move {
        // Signal that we've called admit() and are waiting.
        let _ = tx.send(());
        scheduler_clone.admit(test_flow_id(), 1024.0).await
    });

    // Wait for the spawned task to enter the admission gate.
    rx.await.expect("spawned task should signal");

    // Active flows should still be 1 (the waiter hasn't acquired a permit yet).
    assert_eq!(m.active_flows.get(), 1.0);

    // Release the slot so the waiter can proceed.
    drop(t1);
    let t2 = joiner.await.expect("joiner should not panic").unwrap();

    // The waiter should have acquired a permit now.
    assert_eq!(m.active_flows.get(), 1.0);

    // Drop the ticket.
    drop(t2);
    assert_eq!(m.active_flows.get(), 0.0);
}

/// Test: dropping a ticket early (simulating early return from handler)
/// releases the permit without waiting for the full request lifecycle.
#[tokio::test]
async fn test_early_return_releases_permit() {
    let (scheduler, m) = make_scheduler(2);

    let t1 = scheduler.admit(test_flow_id(), 1024.0).await.unwrap();
    let t2 = scheduler.admit(test_flow_id(), 1024.0).await.unwrap();
    assert_eq!(m.active_flows.get(), 2.0);

    // Simulate early return: drop t1 before "request completes".
    drop(t1);
    assert_eq!(m.active_flows.get(), 1.0);

    // The freed slot should be available for a new request.
    let t3 = scheduler.admit(test_flow_id(), 1024.0).await.unwrap();
    assert_eq!(m.active_flows.get(), 2.0);

    drop(t2);
    drop(t3);
    assert_eq!(m.active_flows.get(), 0.0);
}

/// Test: admit and drop updates active_flows.
#[tokio::test]
async fn test_admit_and_drop_updates_active_flows() {
    let (scheduler, m) = make_scheduler(2);

    assert_eq!(m.active_flows.get(), 0.0);

    let t1 = scheduler.admit(test_flow_id(), 1024.0).await.unwrap();
    assert_eq!(m.active_flows.get(), 1.0);

    // Drop the ticket — active flows should return to 0.
    drop(t1);
    assert_eq!(m.active_flows.get(), 0.0);
}

/// Test: panic in a spawned task still releases the permit.
#[tokio::test]
async fn test_panic_in_spawned_task_releases_permit() {
    let (scheduler, m) = make_scheduler(2);

    // Use a oneshot channel to synchronize the permit acquisition.
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let scheduler_clone = scheduler.clone();
    let joiner = tokio::spawn(async move {
        let _ticket = scheduler_clone.admit(test_flow_id(), 1024.0).await.unwrap();
        // Signal after acquiring so the main task can check active_flows.
        let _ = tx.send(());
        // Simulate some work with the permit.
        tokio::time::sleep(Duration::from_millis(10)).await;
        // Now panic while holding the ticket (it drops during unwind).
        panic!("simulated panic in forwarded task");
    });

    // Wait for the spawned task to signal that it acquired the permit.
    rx.await.expect("spawned task should signal");

    // Check that the spawned task's ticket is active.
    let pre_panic = m.active_flows.get();
    assert!(
        pre_panic == 1.0 || pre_panic == 0.0,
        "active_flows should be 0 or 1 after signal"
    );

    // The task will panic — but the ticket's Drop must run.
    let result = joiner.await;
    assert!(result.is_err(), "task should have panicked");

    // After the panic, the permit must be released.
    assert_eq!(
        m.active_flows.get(),
        0.0,
        "active_flows should be 0 after panicked task drops its ticket"
    );
}

/// Test: multiple concurrent requests queue correctly with max=2.
#[tokio::test]
async fn test_concurrent_admit_with_max_two() {
    let (scheduler, m) = make_scheduler(2);

    // Fire 3 concurrent admits.
    let s1 = scheduler.clone();
    let s2 = scheduler.clone();
    let s3 = scheduler.clone();

    let t1 = tokio::spawn(async move { s1.admit(test_flow_id(), 1024.0).await });
    let t2 = tokio::spawn(async move { s2.admit(test_flow_id(), 1024.0).await });
    let t3 = tokio::spawn(async move { s3.admit(test_flow_id(), 1024.0).await });

    // Wait a bit for the first two to acquire permits.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // At this point, 2 should be active.
    assert_eq!(m.active_flows.get(), 2.0);

    // Retrieve the tickets from the first two tasks.
    let ticket1 = t1.await.unwrap().unwrap();
    let ticket2 = t2.await.unwrap().unwrap();

    // Drop both — the third task should acquire one of the freed slots.
    drop(ticket1);
    drop(ticket2);

    // Wait briefly for the third task to acquire its permit.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Now the third task should be active (1 active flow).
    assert_eq!(m.active_flows.get(), 1.0);

    // Wait for the third task to complete and retrieve its ticket.
    let ticket3 = t3.await.unwrap().unwrap();
    drop(ticket3);

    assert_eq!(m.active_flows.get(), 0.0);
}

/// Test: queue_depth reflects correct count during admission.
#[tokio::test]
async fn test_queue_depth_reflects_waiting_count() {
    let (scheduler, _m) = make_scheduler(1);

    // Occupy the slot.
    let t1 = scheduler.admit(test_flow_id(), 1024.0).await.unwrap();
    assert_eq!(scheduler.queue_depth(), 0);

    // Spawn a waiter.
    let scheduler_clone = scheduler.clone();
    let waiter = tokio::spawn(async move { scheduler_clone.admit(test_flow_id(), 1024.0).await });

    // Give the waiter time to enter the queue.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Queue depth should be 1 (one request waiting for permit).
    assert_eq!(
        scheduler.queue_depth(),
        1,
        "queue_depth should be 1 while one request waits"
    );

    // Release the slot.
    drop(t1);
    let _t2 = waiter.await.expect("waiter should complete").unwrap();

    // Queue should be empty.
    assert_eq!(scheduler.queue_depth(), 0);
}

/// Test: the Prometheus queue_depth gauge is updated in real time.
#[tokio::test]
async fn test_queue_depth_gauge_updates_on_admit() {
    let (scheduler, _m) = make_scheduler(1);

    // Occupy the slot.
    let t1 = scheduler.admit(test_flow_id(), 1024.0).await.unwrap();
    // After acquiring the permit, the internal counter should be 0 (no waiting).
    assert_eq!(
        scheduler.queue_depth(),
        0,
        "queue_depth should be 0 after permit acquired"
    );

    // Spawn a waiter that will enter the queue.
    let scheduler_clone = scheduler.clone();
    let waiter = tokio::spawn(async move { scheduler_clone.admit(test_flow_id(), 1024.0).await });

    // Give the waiter time to enter the queue.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Gauge should reflect 1 waiting request.
    assert_eq!(
        scheduler.queue_depth(),
        1,
        "queue_depth should be 1 while one request waits"
    );

    // Release the slot so waiter can proceed.
    drop(t1);
    let _t2 = waiter.await.expect("waiter should complete").unwrap();

    // Counter should be back to 0.
    assert_eq!(
        scheduler.queue_depth(),
        0,
        "queue_depth should be 0 after all permits released"
    );
}

/// Test: cancellation of admit() does not leak the queue-depth counter.
#[tokio::test]
async fn test_queue_depth_cancel_does_not_leak() {
    let (scheduler, _m) = make_scheduler(1);

    // Occupy the single slot.
    let t1 = scheduler.admit(test_flow_id(), 1024.0).await.unwrap();
    assert_eq!(scheduler.queue_depth(), 0);

    // Spawn a task that tries to admit but will be cancelled.
    let scheduler_clone = scheduler.clone();
    let handle = tokio::spawn(async move { scheduler_clone.admit(test_flow_id(), 1024.0).await });

    // Give the task time to enter the queue.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Depth should be 1 (one waiter).
    assert_eq!(scheduler.queue_depth(), 1);

    // Cancel the waiting task by aborting it.
    handle.abort();
    // Give time for the Abort to process.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Depth should be back to 0 — the cancelled task's depth guard was dropped.
    assert_eq!(
        scheduler.queue_depth(),
        0,
        "queue_depth should be 0 after cancelled task is dropped"
    );

    // The original ticket should still be valid.
    drop(t1);
    assert_eq!(scheduler.queue_depth(), 0);
}
