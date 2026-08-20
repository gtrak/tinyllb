//! Per-flow request-cadence tracking for the interactive-vs-batch priority heuristic.
//!
//! Replaces the median-gap classifier (Plan 004) with a turn-boundary-aware
//! state machine. See `docs/plans/006-turn-boundary-priority/PLAN.md`.
//!
//! States encode both classification and confidence:
//!
//! | State | Priority | Entered when |
//! |---|---|---|
//! | `Cold` | interactive | New flow, no evidence yet (optimistic) |
//! | `Interactive` | interactive | ≥1 turn-boundary idle observed |
//! | `AgenticSuspected` | agent | Continuous arrivals past `agentic_suspected_threshold` |
//! | `AgenticConfirmed` | background | Continuous arrivals past `agentic_confirmed_threshold` |
//!
//! Transitions are reactive: any turn-boundary idle (gap ≥ `idle_gap_threshold`
//! at a `role: user` request) immediately promotes to `Interactive`, regardless
//! of prior state. Continuous non-turn-boundary arrivals increment a counter
//! that drives demotion through `AgenticSuspected` to `AgenticConfirmed`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::config::{Priorities, PriorityPolicy};
use crate::flow::{Flow, FlowId};

/// State-machine state for per-flow cadence classification.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CadenceState {
    /// New flow, no evidence yet. Optimistic: priority = interactive.
    Cold = 0,
    /// ≥1 turn-boundary idle observed. Priority = interactive.
    Interactive = 1,
    /// Continuous arrivals (no idle) past agentic_suspected_threshold.
    /// Priority = agent.
    AgenticSuspected = 2,
    /// Continuous arrivals past agentic_confirmed_threshold.
    /// Priority = background.
    AgenticConfirmed = 3,
}

impl CadenceState {
    /// Map state to numeric priority using the configured Priorities.
    pub fn priority(&self, classes: &Priorities) -> u32 {
        match self {
            CadenceState::Cold => classes.interactive,
            CadenceState::Interactive => classes.interactive,
            CadenceState::AgenticSuspected => classes.agent,
            CadenceState::AgenticConfirmed => classes.background,
        }
    }
}

/// Per-flow cadence state machine.
pub struct Cadence {
    /// Timestamp of the last arrival (for gap computation).
    /// `None` until the first arrival.
    last_arrival: Option<Instant>,
    /// Consecutive arrivals since the last turn boundary (idle or fast).
    /// Resets to 0 on any role:user request. Increments on role:tool
    /// or other non-turn-boundary arrivals.
    continuous_arrival_count: u32,
    /// Current state-machine state.
    state: CadenceState,
    /// Unix timestamp (seconds) of the last arrival. Used by the reaper
    /// to evict entries for flows that stopped sending requests.
    pub last_seen: AtomicU64,
}

impl Cadence {
    pub fn new() -> Self {
        Self {
            last_arrival: None,
            continuous_arrival_count: 0,
            state: CadenceState::Cold,
            last_seen: AtomicU64::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            ),
        }
    }
}

impl Default for Cadence {
    fn default() -> Self {
        Self::new()
    }
}

/// Registry of per-flow cadence state.
// @lat: [[flow#Cadence-Based Priority Heuristic]]
pub struct CadenceRegistry {
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

    /// Record an arrival and update the state machine. Returns the gap
    /// since the previous arrival (for the histogram), or `None` for the
    /// first arrival.
    ///
    /// `is_turn_boundary` is true when the current request's last message
    /// has `role: "user"` or `"system"` (or is non-JSON / non-chat — the
    /// optimistic default). It is false for `role: "tool"` or `"assistant"`.
    pub fn record_arrival(
        &self,
        flow_id: &FlowId,
        now: Instant,
        is_turn_boundary: bool,
    ) -> Option<Duration> {
        let mut entry = self.inner.entry(flow_id.clone()).or_default();
        let prev_gap = entry.last_arrival.map(|last| now.duration_since(last));
        entry.last_arrival = Some(now);
        // Refresh the idle-eviction timestamp.
        entry.last_seen.store(
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            Ordering::Relaxed,
        );

        // State-machine transition.
        let is_idle_chunk =
            is_turn_boundary && prev_gap.map(|g| g >= self.policy.idle_gap_threshold).unwrap_or(false);

        if is_idle_chunk {
            // Turn-boundary idle: promote to Interactive, reset counter.
            entry.state = CadenceState::Interactive;
            entry.continuous_arrival_count = 0;
        } else if is_turn_boundary {
            // Fast turn boundary (role:user but gap < threshold):
            // the user took over, so the continuous agentic run is broken,
            // but without an idle chunk there's no promotion.
            entry.continuous_arrival_count = 0;
            // State unchanged.
        } else {
            // Continuous arrival (role:tool / role:assistant).
            entry.continuous_arrival_count += 1;
            let count = entry.continuous_arrival_count;
            match entry.state {
                CadenceState::Cold | CadenceState::Interactive => {
                    if count >= self.policy.agentic_suspected_threshold {
                        entry.state = CadenceState::AgenticSuspected;
                    }
                }
                CadenceState::AgenticSuspected => {
                    if count >= self.policy.agentic_confirmed_threshold {
                        entry.state = CadenceState::AgenticConfirmed;
                    }
                }
                CadenceState::AgenticConfirmed => {
                    // Already at the floor; stay.
                }
            }
        }

        prev_gap
    }

    /// Classify a flow by cadence and apply the priority if allowed.
    ///
    /// Does nothing if:
    /// - The flow has an explicit priority override (`priority_source != 0`).
    /// - The heuristic is disabled (`policy.enabled == false`).
    ///
    /// The state machine itself provides hysteresis: demotion goes through
    /// `AgenticSuspected` before `AgenticConfirmed`; promotion is immediate
    /// on any idle chunk.
    pub fn classify_and_apply(&self, flow: &Flow, flow_id: &FlowId) {
        // Honor explicit overrides (1 = header, 2 = admin) — do NOT overwrite.
        // Only the heuristic may write when source == 0.
        if flow.priority_source() != 0 {
            return;
        }
        if !self.policy.enabled {
            return;
        }

        let new_priority = {
            let entry = self.inner.entry(flow_id.clone()).or_default();
            entry.state.priority(&self.classes)
        };
        // DashMap guard dropped here.

        flow.set_priority(new_priority);
    }

    /// Return the current cadence state for a flow.
    /// Used by metrics (task 06).
    pub fn state_of(&self, flow_id: &FlowId) -> CadenceState {
        self.inner.entry(flow_id.clone()).or_default().state
    }

    /// Remove cadence entries that haven't been seen in the last `ttl`
    /// seconds. Returns the number of entries removed.
    pub fn reap_idle(&self, ttl: Duration) -> usize {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let cutoff = now.saturating_sub(ttl.as_secs());

        let to_remove: Vec<FlowId> = self
            .inner
            .iter()
            .filter(|entry| entry.value().last_seen.load(Ordering::Relaxed) < cutoff)
            .map(|entry| entry.key().clone())
            .collect();

        for id in &to_remove {
            self.inner.remove(id);
        }
        to_remove.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_registry() -> CadenceRegistry {
        CadenceRegistry::new(
            Arc::new(PriorityPolicy::default()),
            Arc::new(Priorities::default()),
        )
    }

    #[test]
    fn reap_idle_removes_stale_entries() {
        let reg = make_registry();
        let id = FlowId::new("f");
        reg.record_arrival(&id, Instant::now(), false);
        // Age the entry beyond the TTL.
        reg.inner
            .get_mut(&id)
            .unwrap()
            .last_seen
            .store(1, Ordering::Relaxed);

        let removed = reg.reap_idle(Duration::from_secs(600));

        assert_eq!(removed, 1, "stale cadence entry should be removed");
        assert!(!reg.inner.contains_key(&id));
    }

    #[test]
    fn reap_idle_keeps_recent_entries() {
        let reg = make_registry();
        let id = FlowId::new("f");
        reg.record_arrival(&id, Instant::now(), false);

        let removed = reg.reap_idle(Duration::from_secs(600));

        assert_eq!(removed, 0, "recently seen entry must not be reaped");
        assert!(reg.inner.contains_key(&id));
    }

    #[test]
    fn record_arrival_updates_last_seen() {
        let reg = make_registry();
        let id = FlowId::new("f");
        reg.record_arrival(&id, Instant::now(), false);
        let seen = reg
            .inner
            .get(&id)
            .unwrap()
            .last_seen
            .load(Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        assert!(
            seen >= now.saturating_sub(5),
            "last_seen should be set on arrival"
        );
    }
}
