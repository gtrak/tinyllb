use std::time::Duration;

/// Error returned when a request is rejected due to backpressure.
#[derive(Debug, Clone)]
// @lat: [[admission#Backpressure and Admission Rejection]]
pub struct BackpressureRejected {
    /// Suggested retry-after duration for the client.
    pub retry_after: Duration,
}

impl std::fmt::Display for BackpressureRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "queue full, retry after {}s",
            self.retry_after.as_secs_f64()
        )
    }
}

impl std::error::Error for BackpressureRejected {}

/// Compute the `Retry-After` duration for fail-fast mode.
///
/// Formula: `retry_after_base * (1 + depth / max_queue_depth)`
/// When `max_queue_depth` is 0, returns `retry_after_base * 2` to avoid
/// division by zero.
pub fn fail_fast_retry_after(
    depth: u32,
    max_queue_depth: u32,
    retry_after_base: Duration,
) -> Duration {
    let ratio = if max_queue_depth == 0 {
        2.0 // Safeguard: avoid div-by-zero; assume max pressure
    } else {
        depth as f64 / max_queue_depth as f64
    };
    let factor = 1.0 + ratio;
    retry_after_base.mul_f64(factor)
}

/// Human-readable label for `BackpressureMode` used in metrics.
pub fn mode_label(mode: crate::config::BackpressureMode) -> &'static str {
    match mode {
        crate::config::BackpressureMode::Blocking => "blocking",
        crate::config::BackpressureMode::FailFast => "fail_fast",
        crate::config::BackpressureMode::Hybrid => "hybrid",
    }
}
