pub mod cadence;
pub mod identify;

use std::collections::HashSet;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU8, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;

/// Explicit priority class for the `X-LLM-Priority` header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PriorityClass {
    Interactive,
    Agent,
    Background,
}

/// Case-insensitive parse of a priority token.
/// Returns `None` for unknown values (caller logs a warning).
pub fn resolve_priority_token(s: &str) -> Option<PriorityClass> {
    match s.trim().to_ascii_lowercase().as_str() {
        "interactive" => Some(PriorityClass::Interactive),
        "agent" => Some(PriorityClass::Agent),
        "background" => Some(PriorityClass::Background),
        _ => None,
    }
}

/// Result of resolving a request's flow identity + priority override.
pub struct ResolvedFlow {
    /// The resolved flow ID (from existing resolution order).
    pub flow_id: FlowId,
    /// Explicit priority class from `X-LLM-Priority` header, if a valid
    /// class token was sent. `None` when absent or `auto`.
    pub priority_override: Option<PriorityClass>,
    /// True when the client sent `X-LLM-Priority: auto`, requesting the
    /// override be cleared and the heuristic resumed.
    pub unset_override: bool,
}

/// Opaque identifier for a flow.
///
/// Internally a `String`, but `FlowId` is a distinct type to prevent
/// accidental misuse (e.g. passing a raw string where a flow is expected).
#[derive(Clone, PartialEq, Eq, Hash)]
// @lat: [[flow#Flow Identifier Contract]]
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
    /// Weight used for weighted fair scheduling (09+). Stored as `f64` bits
    /// in an `AtomicU64` so that `register` can update in place.
    weight: AtomicU64,
    /// Priority class value (higher = more urgent, 10+).
    priority: AtomicU32,
    /// Per-flow depth: number of requests currently queued/waiting for this flow.
    pub depth: AtomicU32,
    /// Runtime credit for deficit round-robin (11+).
    pub credit: AtomicI64,
    /// When this flow was most recently enqueued (for starvation detection).
    pub enqueued_at: std::sync::RwLock<Option<Instant>>,
    /// Number of currently active (in-flight) requests for this flow.
    pub active: AtomicU32,
    /// Source of the current `priority` value.
    /// 0 = heuristic-derived (default), 1 = X-LLM-Priority header,
    /// 2 = POST /flows admin API.
    pub priority_source: AtomicU8,
}

impl Flow {
    /// Create a new flow with explicit weight and priority.
    pub fn new(id: FlowId, default_weight: f64, default_priority: u32) -> Self {
        Self {
            id,
            weight: AtomicU64::new(default_weight.to_bits()),
            priority: AtomicU32::new(default_priority),
            depth: AtomicU32::new(0),
            credit: AtomicI64::new(0),
            enqueued_at: std::sync::RwLock::new(None),
            active: AtomicU32::new(0),
            priority_source: AtomicU8::new(0),
        }
    }

    /// Read the current weight.
    pub fn weight(&self) -> f64 {
        f64::from_bits(self.weight.load(Ordering::Relaxed))
    }

    /// Set the weight.
    pub fn set_weight(&self, w: f64) {
        self.weight.store(w.to_bits(), Ordering::Relaxed);
    }

    /// Read the current priority.
    pub fn priority(&self) -> u32 {
        self.priority.load(Ordering::Relaxed)
    }

    /// Set the priority.
    pub fn set_priority(&self, p: u32) {
        self.priority.store(p, Ordering::Relaxed);
    }

    /// Read the current priority source.
    pub fn priority_source(&self) -> u8 {
        self.priority_source.load(Ordering::Relaxed)
    }

    /// Set the priority source.
    pub fn set_priority_source(&self, s: u8) {
        self.priority_source.store(s, Ordering::Relaxed);
    }


    /// Check if this flow currently has active (in-flight) requests.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed) > 0
    }

    /// Mark one request for this flow as active (admitted into the backend).
    pub fn inc_active(&self) {
        self.active.fetch_add(1, Ordering::Relaxed);
    }

    /// Mark one request for this flow as no longer active.
    pub fn dec_active(&self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Registration payload for `POST /flows`.
///
/// Sent by the admin API to create or update a flow's weight/priority.
#[derive(Debug, Clone)]
pub struct FlowRegistration {
    pub id: FlowId,
    pub weight: f64,
    pub priority: u32,
}

/// Per-flow entry inside a `QueueSnapshot`.
#[derive(Debug, Clone)]
pub struct QueueFlowEntry {
    pub id: String,
    pub position: u64,
}

/// Snapshot of the current queue state.
#[derive(Debug, Clone)]
pub struct QueueSnapshot {
    pub active: u64,
    pub waiting: u64,
    pub flows: Vec<QueueFlowEntry>,
}

/// Thread-safe registry of flows, keyed by `FlowId`.
///
/// Backed by a `DashMap<FlowId, Arc<Flow>>`.  `get_or_create` returns an
/// `Arc<Flow>` for the given ID, creating one with default weight/priority
/// if it does not already exist.  `register` upserts with explicit values.
// @lat: [[flow#Flow Registry and State]]
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

    /// Register (upsert) a flow with explicit weight and priority.
    ///
    /// Returns `true` if this was a new registration, `false` if an existing
    /// flow was updated.
    pub fn register(&self, reg: FlowRegistration) -> bool {
        let id = reg.id.clone();
        let weight_bits = reg.weight.to_bits();
        let priority = reg.priority;

        // Try to get existing entry; if exists, update in place.
        if let Some(entry) = self.flows.get_mut(&id) {
            entry.value().weight.store(weight_bits, Ordering::Relaxed);
            entry.value().priority.store(priority, Ordering::Relaxed);
            entry.value().priority_source.store(2, Ordering::Relaxed);
            false // updated
        } else {
            let flow = Arc::new(Flow {
                id: reg.id,
                weight: AtomicU64::new(weight_bits),
                priority: AtomicU32::new(priority),
                depth: AtomicU32::new(0),
                credit: AtomicI64::new(0),
                enqueued_at: std::sync::RwLock::new(None),
                active: AtomicU32::new(0),
                priority_source: AtomicU8::new(2),
            });
            self.flows.insert(FlowId::new(id.to_string()), flow);
            true // created
        }
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

    /// Build a snapshot of flows currently waiting (depth > 0).
    ///
    /// Takes `active` (from the scheduler's active gauge) and `waiting`
    /// (total queue depth).  `flows` lists only waiting flows, ordered
    /// by the supplied `wait_order` iterator (queue position).
    pub fn queue_snapshot<I>(&self, active: u64, waiting: u64, wait_order: I) -> QueueSnapshot
    where
        I: IntoIterator<Item = FlowId>,
    {
        let mut seen: HashSet<String> = HashSet::new();
        let mut flows = Vec::new();
        let mut position: u64 = 1;

        for flow_id in wait_order {
            let id_str = flow_id.to_string();
            if seen.contains(&id_str) {
                continue;
            }
            if let Some(entry) = self.flows.get(&flow_id) {
                if entry.value().depth.load(Ordering::Relaxed) > 0 {
                    seen.insert(id_str);
                    flows.push(QueueFlowEntry {
                        id: flow_id.to_string(),
                        position,
                    });
                    position += 1;
                }
            }
        }

        QueueSnapshot {
            active,
            waiting,
            flows,
        }
    }

    /// Apply (or clear) an explicit priority override on a flow.
    ///
    /// - `unset == true`: clear any prior pin, set `priority_source = 0`
    ///   (heuristic resumes), and reset priority to `classes.agent` (the
    ///   default). Idempotent — safe to call when no pin was set.
    /// - `override_class == Some(c)`: set priority to the class's numeric
    ///   value and `priority_source = 1` (header).
    /// - `override_class == None && !unset`: no header in this request,
    ///   keep existing state (the heuristic or a prior pin stays in effect).
    pub fn apply_priority_override(
        &self,
        flow_id: &FlowId,
        override_class: Option<PriorityClass>,
        unset: bool,
        classes: &crate::config::Priorities,
    ) {
        let flow = self.get_or_create(flow_id.clone());
        if unset {
            flow.set_priority_source(0);
            flow.set_priority(classes.agent);
            return;
        }
        if let Some(class) = override_class {
            let v = match class {
                PriorityClass::Interactive => classes.interactive,
                PriorityClass::Agent => classes.agent,
                PriorityClass::Background => classes.background,
            };
            flow.set_priority(v);
            flow.set_priority_source(1); // header
        }
        // None && !unset: no header, keep state.
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
