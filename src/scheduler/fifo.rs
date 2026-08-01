use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::BackpressureMode;
use crate::metrics::Metrics;
use crate::scheduler::backpressure::{fail_fast_retry_after, BackpressureRejected};

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
/// Requests call `admit()` which may block, reject, or timeout depending on
/// the configured backpressure mode. At most `max_active_flows` requests
/// proceed simultaneously.
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
    semaphore: Arc<tokio::sync::Semaphore>,
    /// Shared metrics handle.
    metrics: Arc<Metrics>,
    /// Backpressure mode.
    backpressure_mode: BackpressureMode,
    /// Max queue depth for fail-fast check.
    max_queue_depth: u32,
    /// Max wait duration for hybrid mode.
    max_wait: Duration,
    /// Base duration for Retry-After computation.
    retry_after_base: Duration,
}

impl FifoScheduler {
    /// Create a new FIFO scheduler with the given max active flows and
    /// backpressure configuration.
    pub fn new(
        max_active_flows: u32,
        metrics: Arc<Metrics>,
        backpressure_mode: BackpressureMode,
        max_queue_depth: u32,
        max_wait: Duration,
        retry_after_base: Duration,
    ) -> Self {
        Self {
            queue_depth: Arc::new(AtomicU32::new(0)),
            semaphore: Arc::new(tokio::sync::Semaphore::new(max_active_flows as usize)),
            metrics,
            backpressure_mode,
            max_queue_depth,
            max_wait,
            retry_after_base,
        }
    }

    /// Attempt to admit a request into the active set.
    ///
    /// Behavior depends on the configured backpressure mode:
    /// - **Blocking**: queue indefinitely until a permit is available.
    /// - **FailFast**: if queue depth > max_queue_depth, return `BackpressureRejected`
    ///   immediately. Otherwise, behave like Blocking.
    /// - **Hybrid**: wait up to `max_wait` for a permit. If the wait
    ///   exceeds `max_wait`, return `BackpressureRejected`.
    pub async fn admit(&self) -> Result<QueueTicket, BackpressureRejected> {
        match self.backpressure_mode {
            BackpressureMode::Blocking => self.admit_blocking().await,
            BackpressureMode::FailFast => self.admit_fail_fast().await,
            BackpressureMode::Hybrid => self.admit_hybrid().await,
        }
    }

    /// Blocking mode: identical to pre-issue-06 behavior.
    async fn admit_blocking(&self) -> Result<QueueTicket, BackpressureRejected> {
        let enter = Instant::now();

        let mut depth_guard = DepthGuard::new(self.queue_depth.clone(), self.metrics.clone());

        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore should not be closed; we never close it");

        depth_guard.consume();
        record_wait_and_active(self, enter);
        Ok(make_ticket(permit, self.metrics.clone()))
    }

    /// Fail-fast mode: reject immediately if the queue is too deep.
    async fn admit_fail_fast(&self) -> Result<QueueTicket, BackpressureRejected> {
        let depth = self.queue_depth.load(Ordering::Relaxed);
        if depth > self.max_queue_depth {
            let retry_after =
                fail_fast_retry_after(depth, self.max_queue_depth, self.retry_after_base);
            return Err(BackpressureRejected { retry_after });
        }

        // Otherwise proceed with blocking behavior.
        let enter = Instant::now();

        let mut depth_guard = DepthGuard::new(self.queue_depth.clone(), self.metrics.clone());

        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore should not be closed; we never close it");

        depth_guard.consume();
        record_wait_and_active(self, enter);
        Ok(make_ticket(permit, self.metrics.clone()))
    }

    /// Hybrid mode: race permit acquisition against a timeout.
    async fn admit_hybrid(&self) -> Result<QueueTicket, BackpressureRejected> {
        let enter = Instant::now();

        let mut depth_guard = DepthGuard::new(self.queue_depth.clone(), self.metrics.clone());

        // Use biased select so that if both branches are ready, the acquire
        // branch wins (no spurious rejection). The `acquire_owned` future
        // is polled first.
        let permit = tokio::select!(
            biased;

            // Acquire the semaphore permit.
            permit = self.semaphore.clone().acquire_owned() => {
                permit.expect("semaphore should not be closed; we never close it")
            }

            // Timeout: if we haven't acquired in time, reject.
            _ = tokio::time::sleep(self.max_wait) => {
                // depth_guard is dropped here, correctly decrementing queue_depth.
                let depth = self.queue_depth.load(Ordering::Relaxed);
                let retry_after =
                    fail_fast_retry_after(depth, self.max_queue_depth, self.retry_after_base);
                return Err(BackpressureRejected { retry_after });
            }
        );

        depth_guard.consume();
        record_wait_and_active(self, enter);
        Ok(make_ticket(permit, self.metrics.clone()))
    }

    /// Current number of requests inside `admit()` (waiting for a permit).
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

/// Record the wait time and increment active flows.
fn record_wait_and_active(scheduler: &FifoScheduler, enter: Instant) {
    let wait_secs = enter.elapsed().as_secs_f64();
    scheduler.metrics.queue_wait_seconds.observe(wait_secs);
    scheduler.metrics.active_flows.inc();
}

/// Construct a `QueueTicket` from a permit and metrics handle.
fn make_ticket(permit: tokio::sync::OwnedSemaphorePermit, metrics: Arc<Metrics>) -> QueueTicket {
    QueueTicket {
        _permit: permit,
        metrics,
    }
}
