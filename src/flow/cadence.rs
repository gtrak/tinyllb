//! Per-flow request-cadence tracking for the interactive-vs-batch priority heuristic.
//!
//! See `docs/plans/004-interactive-priority-heuristic/PLAN.md`.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::config::{Priorities, PriorityPolicy};
use crate::flow::{Flow, FlowId};

/// Rolling per-flow arrival history.
pub struct Cadence {
    arrivals: VecDeque<Instant>,
}

impl Cadence {
    pub fn new() -> Self {
        Self {
            arrivals: VecDeque::new(),
        }
    }

    /// Record a new arrival timestamp and evict old entries beyond the window.
    pub fn record_arrival(&mut self, now: Instant, window: usize) {
        self.arrivals.push_back(now);
        while self.arrivals.len() > window {
            self.arrivals.pop_front();
        }
    }

    /// Compute the median inter-request gap over the current arrival history.
    ///
    /// If fewer than 2 samples, returns `None`. Otherwise computes consecutive
    /// deltas (`arrivals[i+1] - arrivals[i]`), sorts a copy of them, and returns
    /// the median. For even-length sequences we pick the lower-middle element
    /// (index `len / 2 - 1`) to be deterministic and conservative.
    pub fn median_gap(&self) -> Option<Duration> {
        if self.arrivals.len() < 2 {
            return None;
        }

        let mut deltas: Vec<Duration> = Vec::with_capacity(self.arrivals.len() - 1);
        let arr: Vec<Instant> = self.arrivals.iter().copied().collect();
        for i in 0..arr.len() - 1 {
            deltas.push(arr[i + 1] - arr[i]);
        }
        deltas.sort();

        // For even counts, pick lower-middle (deterministic per the doc comment).
        // For odd counts, pick the exact middle.
        let mid = if deltas.len().is_multiple_of(2) {
            deltas.len() / 2 - 1
        } else {
            deltas.len() / 2
        };
        Some(deltas[mid])
    }

    /// Classify this flow into a priority class based on observed cadence.
    ///
    /// Returns `None` when there are fewer than `policy.min_samples` arrivals
    /// (cold start — leave priority alone). Otherwise returns a numeric priority:
    ///
    /// - `gap <= policy.background_gap_max` → `classes.background` (10)
    /// - `gap >= policy.interactive_gap_min` → `classes.interactive` (100)
    /// - in between → `classes.agent` (50)
    ///
    /// Boundary semantics: `== background_gap_max` is background;
    /// `== interactive_gap_min` is interactive.
    pub fn classify(&self, policy: &PriorityPolicy, classes: &Priorities) -> Option<u32> {
        if self.arrivals.len() < policy.min_samples {
            return None;
        }

        let gap = self.median_gap()?;

        if gap <= policy.background_gap_max {
            Some(classes.background)
        } else if gap >= policy.interactive_gap_min {
            Some(classes.interactive)
        } else {
            Some(classes.agent)
        }
    }

    /// Check if the last `k` consecutive gaps are all `<= threshold`.
    ///
    /// Computes the deltas among the most recent `k+1` arrivals.
    /// Returns `false` if there are fewer than `k+1` arrivals (not enough data
    /// to confirm a sustained burst).
    pub fn last_k_gap_all_le(&self, threshold: Duration, k: usize) -> bool {
        let needed = k + 1;
        if self.arrivals.len() < needed {
            return false;
        }

        let arr: Vec<Instant> = self.arrivals.iter().copied().collect();
        let start = arr.len() - needed;
        for i in start..start + k {
            let gap = arr[i + 1] - arr[i];
            if gap > threshold {
                return false;
            }
        }
        true
    }
}

impl Default for Cadence {
    fn default() -> Self {
        Self::new()
    }
}

/// Registry of per-flow cadence state.
pub struct CadenceRegistry {
    /// Changed from `Arc<Cadence>` to plain `Cadence` so that `DashMap::entry`
    /// returns a `RefMut` guard that allows direct in-place mutation of `arrivals`.
    inner: DashMap<FlowId, Cadence>,
    policy: Arc<PriorityPolicy>,
    classes: Arc<Priorities>,
}

impl CadenceRegistry {
    pub fn new(policy: Arc<PriorityPolicy>, classes: Arc<Priorities>) -> Self {
        Self {
            inner: DashMap::new(),
            policy,
            classes,
        }
    }

    /// Record an arrival for a flow. Called by the scheduler on every admit.
    pub fn record_arrival(&self, flow_id: &FlowId, now: Instant) {
        let mut entry = self.inner.entry(flow_id.clone()).or_default();
        entry.record_arrival(now, self.policy.sample_window);
    }

    /// Classify a flow by cadence and apply the priority if allowed.
    ///
    /// Does nothing if:
    /// - The flow has an explicit priority override (`priority_source != 0`).
    /// - The heuristic is disabled (`policy.enabled == false`).
    /// - There are fewer than `min_samples` arrivals (cold start).
    ///
    /// Hysteresis: when demoting from interactive to a lower class, requires
    /// that the last 3 gaps are ALL fast (`<= background_gap_max`) to prevent
    /// a single burst of quick follow-ups from losing interactive priority.
    pub fn classify_and_apply(&self, flow: &Flow, flow_id: &FlowId) {
        // Honor explicit overrides (1 = header, 2 = admin) — do NOT overwrite.
        // Only the heuristic may write when source == 0.
        if flow.priority_source() != 0 {
            return;
        }
        if !self.policy.enabled {
            return;
        }

        // Read classification while holding the DashMap guard.
        let (new_class, hysteresis_ok) = {
            let entry = self.inner.entry(flow_id.clone()).or_default();
            let new_class = entry.classify(&self.policy, &self.classes);
            let hysteresis_ok = if let Some(new) = new_class {
                let current = flow.priority();
                if new < current && current == self.classes.interactive {
                    entry.last_k_gap_all_le(self.policy.background_gap_max, 3)
                } else {
                    true
                }
            } else {
                true
            };
            (new_class, hysteresis_ok)
        };
        // DashMap guard is dropped here — safe to call flow.set_priority.

        let Some(new) = new_class else {
            return; // Cold start — keep current priority.
        };

        let current = flow.priority();
        // Hysteresis: only demote an interactive flow if the last 3 gaps are ALL fast.
        if new < current && current == self.classes.interactive && !hysteresis_ok {
            return;
        }

        flow.set_priority(new);
    }
}
