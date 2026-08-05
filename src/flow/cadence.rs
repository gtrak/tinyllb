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
}

impl Cadence {
    pub fn new() -> Self {
        Self {
            last_arrival: None,
            continuous_arrival_count: 0,
            state: CadenceState::Cold,
        }
    }
}

impl Default for Cadence {
    fn default() -> Self {
        Self::new()
    }
}

/// Registry of per-flow cadence state.
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
}
