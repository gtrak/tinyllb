use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Semaphore;

use crate::metrics::Metrics;

/// RAII guard for the queue-depth atomic counter.
///
/// Increments `queue_depth` by 1 when created and decrements it when dropped.
/// Also sets the Prometheus `llm_queue_depth` gauge after each change.
///
/// On the success path the guard is "consumed" (permit acquired), so the depth
/// is set back to the correct value and the guard is nulled out — its Drop
/// becomes a no-op.  On the cancellation path the guard is simply dropped,
/// correctly releasing the depth increment.
struct DepthGuard {
    depth: Arc<AtomicU32>,
    metrics: Arc<Metrics>,
    active: bool, // false after consume()
}

impl DepthGuard {
    fn new(depth: Arc<AtomicU32>, metrics: Arc<Metrics>) -> Self {
        let val = depth.fetch_add(1, Ordering::Relaxed) + 1;
        metrics.queue_depth.set(val as f64);
        Self {
            depth,
            metrics,
            active: true,
        }
    }

    /// Called when the permit is acquired: decrement depth, update gauge,
    /// and nullify the guard so Drop is a no-op.
    fn consume(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let val = self.depth.fetch_sub(1, Ordering::Relaxed).saturating_sub(1);
        self.metrics.queue_depth.set(val as f64);
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let val = self.depth.fetch_sub(1, Ordering::Relaxed).saturating_sub(1);
        self.metrics.queue_depth.set(val as f64);
    }
}

/// RAII ticket returned by `FifoScheduler::admit`.
///
/// Holds an `OwnedSemaphorePermit` and a reference to the shared metrics.
/// When dropped, it:
/// 1. Releases the semaphore permit (adding it back to the pool).
/// 2. Decrements `llm_active_flows`.
///
/// This guarantees slot release on **all** exit paths: success, error,
/// panic (Drop runs on unwind), and client disconnect (future handler drops).
pub struct QueueTicket {
    _permit: tokio::sync::OwnedSemaphorePermit,
    metrics: Arc<Metrics>,
}

/// FIFO scheduler with a max-active-flows admission gate.
///
/// Requests call `admit()` which blocks until a semaphore permit is
/// available. At most `max_active_flows` requests proceed simultaneously.
///
/// Metrics updated:
/// - `llm_queue_depth`: +1 when entering `admit()`, -1 when permit acquired.
/// - `llm_queue_wait_seconds`: observed when the permit is acquired (wall
///   clock from entry to acquire). Instantaneous grants observe ~0.
/// - `llm_active_flows`: +1 inside `QueueTicket` on permit acquire,
///   -1 when the ticket is dropped.
pub struct FifoScheduler {
    /// Number of requests currently inside `admit()` (waiting or holding).
    /// Used for the `llm_queue_depth` gauge.
    queue_depth: Arc<AtomicU32>,
    /// Semaphore limiting concurrent active flows.
    semaphore: Arc<Semaphore>,
    /// Shared metrics handle.
    metrics: Arc<Metrics>,
}

impl FifoScheduler {
    /// Create a new FIFO scheduler with the given max active flows.
    pub fn new(max_active_flows: u32, metrics: Arc<Metrics>) -> Self {
        Self {
            queue_depth: Arc::new(AtomicU32::new(0)),
            semaphore: Arc::new(Semaphore::new(max_active_flows as usize)),
            metrics,
        }
    }

    /// Attempt to admit a request into the active set.
    ///
    /// Blocks until a permit is available (blocking mode — issue 06 will
    /// add fail-fast / 429 paths). On success returns a `QueueTicket` that
    /// must be held for the duration of the forwarded request.
    pub async fn admit(&self) -> QueueTicket {
        let enter = Instant::now();

        // Create a depth guard: increments queue_depth + sets gauge.
        // If the future is cancelled, the guard's Drop decrements depth.
        let mut depth_guard = DepthGuard::new(self.queue_depth.clone(), self.metrics.clone());

        // Acquire a permit. Blocks if all permits are in use.
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore should not be closed; we never close it");

        // Permit acquired — consume the depth guard (decrement + set gauge).
        depth_guard.consume();

        // Record wait time from entry to permit acquisition.
        let wait_secs = enter.elapsed().as_secs_f64();
        self.metrics.queue_wait_seconds.observe(wait_secs);

        // Active flow: increment now; QueueTicket::Drop will decrement.
        self.metrics.active_flows.inc();

        QueueTicket {
            _permit: permit,
            metrics: self.metrics.clone(),
        }
    }

    /// Current number of requests inside `admit()` (waiting for a permit).
    ///
    /// Used to update `llm_queue_depth`. In practice the atomic is updated
    /// inline (increment on entry, decrement on permit acquire) and this
    /// accessor is primarily for observability / testing.
    pub fn queue_depth(&self) -> u32 {
        self.queue_depth.load(Ordering::Relaxed)
    }
}

impl Drop for QueueTicket {
    fn drop(&mut self) {
        // The permit is dropped by the `_permit` field, releasing it back
        // to the semaphore. We also decrement the active_flows gauge here.
        self.metrics.active_flows.dec();
    }
}
