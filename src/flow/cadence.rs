//! Per-flow request-cadence tracking for the interactive-vs-batch priority heuristic.
//!
//! See `docs/plans/004-interactive-priority-heuristic/PLAN.md`.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;

use crate::config::{Priorities, PriorityPolicy};
use crate::flow::FlowId;

/// Rolling per-flow arrival history.
pub struct Cadence {
    #[allow(dead_code)] // populated in task 02
    arrivals: VecDeque<Instant>,
}

impl Cadence {
    pub fn new() -> Self {
        Self {
            arrivals: VecDeque::new(),
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
    #[allow(dead_code)] // consumed in task 04
    inner: DashMap<FlowId, Arc<Cadence>>,
    #[allow(dead_code)] // consumed in task 02
    policy: Arc<PriorityPolicy>,
    #[allow(dead_code)] // consumed in task 02
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
}
