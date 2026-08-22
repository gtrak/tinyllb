//! Stateful llama.cpp slot allocation for `id_slot` session pinning.
//!
//! The allocator pins each named flow to a stable slot for the lifetime of
//! its flow-registry entry. A new flow takes its deterministic hash "home"
//! slot when it is free, otherwise the lowest free slot, so two live flows
//! never share a slot while capacity allows. When the backend has fewer
//! slots than live flows (saturation), a new flow falls back to its hash
//! home slot and shares it — the pre-stateful behavior under load.
//!
//! Assignments are in-memory only. After a proxy restart the first request
//! of each flow re-derives its hash home, so the common (collision-free)
//! case reproduces the original deterministic mapping and any warm server
//! KV cache stays usable.

use std::collections::HashMap;
use std::sync::Mutex;

use super::{slot_id_for_flow, FlowId};

// @lat: [[gateway#Session Slot Pinning]]
/// Stateful flow→slot allocator backing `id_slot` session pinning.
///
/// Concurrent-safe: every operation takes a short internal mutex (no I/O
/// inside the critical section).
pub struct SlotAllocator {
    inner: Mutex<Inner>,
}

struct Inner {
    /// Flow → assigned slot, held for the flow's registered lifetime.
    assignments: HashMap<FlowId, u32>,
    /// Slot → number of flows currently assigned to it. A count above 1
    /// only occurs under saturation (more live flows than slots), when
    /// assignment degrades to the deterministic hash fallback.
    occupied: HashMap<u32, u32>,
}

impl Inner {
    /// A slot is free when no flow is assigned to it.
    fn is_free(&self, slot: u32) -> bool {
        self.occupied.get(&slot).is_none_or(|&count| count == 0)
    }

    /// Decrement a slot's occupancy, dropping the entry when it hits zero.
    fn free(&mut self, slot: u32) {
        if let Some(count) = self.occupied.get_mut(&slot) {
            *count -= 1;
            if *count == 0 {
                self.occupied.remove(&slot);
            }
        }
    }
}

impl SlotAllocator {
    /// Create an empty allocator (no flows assigned).
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                assignments: HashMap::new(),
                occupied: HashMap::new(),
            }),
        }
    }

    /// Resolve (and record) the slot for `flow` given the live slot count.
    ///
    /// Stable for the flow's lifetime: an existing assignment is returned
    /// unchanged unless it is stale (its index is out of range because the
    /// slot count shrank), in which case the flow is re-assigned. A new
    /// flow takes its hash home slot when free, else the lowest free slot;
    /// if no slot is free (saturation) it takes its hash home slot anyway.
    ///
    /// # Panics
    ///
    /// `slot_count` must be >= 1; the caller (the proxy) gates on it.
    pub fn assign(&self, flow: &FlowId, slot_count: u32) -> u32 {
        debug_assert!(slot_count >= 1, "slot_count must be >= 1");
        let mut guard = self.inner.lock().unwrap();
        if let Some(&slot) = guard.assignments.get(flow) {
            if slot < slot_count {
                return slot;
            }
            // Stale after a slot-count shrink: drop and re-assign below.
            guard.assignments.remove(flow);
            guard.free(slot);
        }
        let home = slot_id_for_flow(&flow.to_string(), slot_count);
        let slot = if guard.is_free(home) {
            home
        } else {
            (0..slot_count).find(|&s| guard.is_free(s)).unwrap_or(home)
        };
        guard.assignments.insert(flow.clone(), slot);
        *guard.occupied.entry(slot).or_insert(0) += 1;
        slot
    }

    /// Release a flow's slot, called when the flow is reaped. A no-op when
    /// the flow holds no assignment (ephemeral flows never get one).
    pub fn release(&self, flow: &FlowId) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(slot) = guard.assignments.remove(flow) {
            guard.free(slot);
        }
    }

    /// Drop assignments for flows that no longer exist in the caller's
    /// registry (defensive leak guard). Returns the number dropped.
    pub fn sweep_missing(&self, exists: impl Fn(&FlowId) -> bool) -> usize {
        let mut guard = self.inner.lock().unwrap();
        let orphans: Vec<(FlowId, u32)> = guard
            .assignments
            .iter()
            .filter(|(id, _)| !exists(id))
            .map(|(id, &slot)| (id.clone(), slot))
            .collect();
        for (id, slot) in &orphans {
            guard.assignments.remove(id);
            guard.free(*slot);
        }
        orphans.len()
    }

    /// Number of flows currently holding an assignment.
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().assignments.len()
    }

    /// Whether no flow holds an assignment.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for SlotAllocator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hash home of `flow` under `n` slots.
    fn home(flow: &FlowId, n: u32) -> u32 {
        slot_id_for_flow(&flow.to_string(), n)
    }

    /// Find two distinct flow ids whose hash home slots collide mod `n`.
    /// FNV-1a is deterministic, so a collision always exists for small `n`.
    fn colliding_pair(n: u32) -> (FlowId, FlowId) {
        for i in 0..100_000u32 {
            let a = FlowId::new(format!("alpha-{i}"));
            for j in 0..100_000u32 {
                let b = FlowId::new(format!("beta-{j}"));
                if home(&a, n) == home(&b, n) {
                    return (a, b);
                }
            }
        }
        unreachable!("FNV-1a must produce a colliding pair mod {n}")
    }

    #[test]
    fn assign_is_stable_per_flow() {
        let alloc = SlotAllocator::new();
        let flow = FlowId::new("stable");
        let first = alloc.assign(&flow, 8);
        for _ in 0..16 {
            assert_eq!(alloc.assign(&flow, 8), first);
        }
    }

    #[test]
    fn colliding_homes_get_distinct_slots() {
        let alloc = SlotAllocator::new();
        let (a, b) = colliding_pair(2);
        let sa = alloc.assign(&a, 2);
        let sb = alloc.assign(&b, 2);
        assert_eq!(sa, home(&a, 2), "the first flow keeps its hash home");
        assert_ne!(sa, sb, "a colliding flow must be placed on a different slot");
        assert!(sb < 2);
    }

    #[test]
    fn released_slot_is_reused() {
        let alloc = SlotAllocator::new();
        let (a, b) = colliding_pair(2);
        let sa = alloc.assign(&a, 2);
        let sb = alloc.assign(&b, 2);
        alloc.release(&a);
        let sa_again = alloc.assign(&a, 2);
        assert_eq!(sa_again, sa, "a returning flow retakes its freed home");
        assert_ne!(sa_again, sb);
        assert_eq!(alloc.len(), 2);
    }

    #[test]
    fn saturation_falls_back_to_hash_home() {
        let alloc = SlotAllocator::new();
        let (a, b) = colliding_pair(2);
        alloc.assign(&a, 2);
        alloc.assign(&b, 2);
        let c = FlowId::new("overflow-flow");
        assert_eq!(
            alloc.assign(&c, 2),
            home(&c, 2),
            "with no free slot, assignment degrades to the hash home"
        );
    }

    #[test]
    fn shrunken_slot_count_reassigns() {
        let alloc = SlotAllocator::new();
        let flow = (0..1000u32)
            .map(|i| FlowId::new(format!("shrink-{i}")))
            .find(|f| home(f, 8) != 0)
            .expect("some flow must hash away from slot 0");
        let s8 = alloc.assign(&flow, 8);
        assert_ne!(s8, 0);
        let s1 = alloc.assign(&flow, 1);
        assert_eq!(s1, 0, "a stale (out-of-range) assignment must be re-assigned");
    }

    #[test]
    fn sweep_missing_drops_orphaned_assignments() {
        let alloc = SlotAllocator::new();
        let (a, b) = colliding_pair(2);
        alloc.assign(&a, 2);
        alloc.assign(&b, 2);
        let dropped = alloc.sweep_missing(|id| id == &a);
        assert_eq!(dropped, 1);
        assert_eq!(alloc.len(), 1);
        // b keeps its slot; a returning flow still gets a distinct one.
        let sb = alloc.assign(&b, 2);
        let sa = alloc.assign(&a, 2);
        assert_ne!(sa, sb);
    }
}
