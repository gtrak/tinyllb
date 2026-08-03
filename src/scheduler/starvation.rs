//! Starvation protection helpers.
//!
//! Provides utilities for detecting starved flows and recording starvation
//! metrics.  The actual starvation check is performed inline in each
//! scheduler's `try_select`, and the completion bias gate also checks for
//! starvation to allow starved flows through the gate.

use std::time::{Duration, Instant};

use crate::flow::Flow;
use crate::metrics::Metrics;

/// Record a starvation force-admit event for the given flow.
///
/// Sets `llm_flow_starvation_seconds{flow_id}` to the observed wait time
/// and increments `llm_starvation_force_admits_total`.
pub fn record_force_admit(metrics: &Metrics, flow: &Flow, wait: Duration) {
    metrics
        .flow_starvation_seconds
        .with_label_values(&[flow.id.metric_label()])
        .set(wait.as_secs_f64());
    metrics.starvation_force_admits_total.inc();
}

/// Check if a flow is currently starved (enqueued for longer than the timeout).
///
/// Returns `Some(wait_duration)` if starved, `None` otherwise.
// @lat: [[scheduler_policies#Starvation Protection]]
pub fn is_starved(flow: &Flow, timeout: Duration) -> Option<Duration> {
    let enqueued_at = flow.enqueued_at.read().unwrap();
    if let Some(queued_at) = *enqueued_at {
        let wait = Instant::now().duration_since(queued_at);
        if wait > timeout {
            return Some(wait);
        }
    }
    None
}
