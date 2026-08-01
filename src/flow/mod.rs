pub mod identify;

use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;

/// Opaque identifier for a flow.
///
/// Internally a `String`, but `FlowId` is a distinct type to prevent
/// accidental misuse (e.g. passing a raw string where a flow is expected).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct FlowId(String);

impl FlowId {
    /// Create a new flow ID from a string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns `true` if this is an ephemeral (auto-generated) flow ID.
    pub fn is_ephemeral(&self) -> bool {
        self.0.starts_with("ephemeral-")
    }

    /// Return a label value suitable for Prometheus.
    ///
    /// Ephemeral flows aggregate to `"ephemeral"` to avoid cardinality
    /// explosion; named flows return their actual ID.
    pub fn metric_label(&self) -> &str {
        if self.is_ephemeral() {
            "ephemeral"
        } else {
            &self.0
        }
    }
}

impl std::fmt::Debug for FlowId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FlowId({})", self.0)
    }
}

impl std::fmt::Display for FlowId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A flow represents a logical client or workload whose requests are
/// scheduled together.
#[derive(Debug)]
pub struct Flow {
    /// Unique identifier for this flow.
    pub id: FlowId,
    /// Weight used for weighted fair scheduling (09+).
    pub weight: f64,
    /// Priority class value (higher = more urgent, 10+).
    pub priority: u32,
    /// Per-flow depth: number of requests currently queued/waiting for this flow.
    pub depth: AtomicU32,
    /// Runtime credit for deficit round-robin (11+).
    pub credit: AtomicI64,
    /// When this flow was most recently enqueued (for starvation detection).
    pub enqueued_at: std::sync::RwLock<Option<Instant>>,
}

impl Flow {
    /// Create a new flow with default weight and priority.
    pub fn new(id: FlowId, default_weight: f64, default_priority: u32) -> Self {
        Self {
            id,
            weight: default_weight,
            priority: default_priority,
            depth: AtomicU32::new(0),
            credit: AtomicI64::new(0),
            enqueued_at: std::sync::RwLock::new(None),
        }
    }
}

/// Thread-safe registry of flows, keyed by `FlowId`.
///
/// Backed by a `DashMap<FlowId, Arc<Flow>>`.  `get_or_create` returns an
/// `Arc<Flow>` for the given ID, creating one with default weight/priority
/// if it does not already exist.
pub struct FlowRegistry {
    flows: DashMap<FlowId, Arc<Flow>>,
    default_weight: f64,
    default_priority: u32,
}

impl FlowRegistry {
    /// Create a new empty registry with the given default weight and priority.
    pub fn new(default_weight: f64, default_priority: u32) -> Self {
        Self {
            flows: DashMap::new(),
            default_weight,
            default_priority,
        }
    }

    /// Return the existing flow for `id`, or create one with defaults.
    ///
    /// Uses DashMap's `entry()` API for atomic check-and-insert, preventing
    /// TOCTOU races when concurrent first-time creations race.
    pub fn get_or_create(&self, id: FlowId) -> Arc<Flow> {
        let dw = self.default_weight;
        let dp = self.default_priority;
        let flow_id = id.clone();
        self.flows
            .entry(id)
            .or_insert_with(|| Arc::new(Flow::new(flow_id, dw, dp)))
            .clone()
    }

    /// Return the number of registered flows.
    pub fn len(&self) -> usize {
        self.flows.len()
    }

    /// Return `true` if no flows are registered.
    pub fn is_empty(&self) -> bool {
        self.flows.is_empty()
    }

    /// Sum the per-flow depth counters across all registered flows.
    pub fn sum_depths(&self) -> u32 {
        self.flows
            .iter()
            .map(|entry| entry.value().depth.load(Ordering::Relaxed))
            .sum()
    }
}

impl std::fmt::Debug for FlowRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlowRegistry")
            .field("flows", &self.flows.len())
            .field("default_weight", &self.default_weight)
            .field("default_priority", &self.default_priority)
            .finish()
    }
}
