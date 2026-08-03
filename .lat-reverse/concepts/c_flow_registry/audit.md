# Audit: FlowRegistry (c_flow_registry)

**Spec:** `.lat-reverse/concepts/c_flow_registry/spec.md` (twice-corrected)
**Source:** `src/flow/mod.rs`
**Date:** 2026-08-03
**Auditor:** Final-cycle Auditor, LAT Reconstruction Pipeline

---

## "No How" Lint

The spec is checked against the four forbidden categories: control flow descriptions, data structure details, function/method names as concept identifiers, and implementation-specific terminology.

| # | Section | Quoted Text | Violation | Severity |
|---|---------|------------|-----------|----------|
| N1 | Per-Flow Attributes | "with their own **atomic or locked** mutation surfaces" | Implementation-specific terminology / data structure detail | **Reject** |
| N2 | Constraints | "The active-count decrement uses **unsaturated subtraction on an unsigned counter**" | Implementation-specific terminology / data structure detail | **Reject** |
| N3 | Constraints | "in **debug builds**... in **release builds**..." | Implementation-specific terminology (compilation profile) | **Reject** |
| N4 | Purpose | "provides concurrent access to **independently usable flow references**" | Borderline: "independently usable" is contractual, but "flow references" hints at Arc. Acceptable with caution. | **Warn** |

**N1** — The spec must not tell the reader that the mutation surface is "atomic" or "locked." The contract is: "each counter attribute is independently mutable and safe for concurrent access." Replace implementation mechanism with the observable guarantee.

**N2** — "Unsaturated subtraction" and "unsigned counter" describe the mechanism, not the contract. Replace with: "Decrementing the active counter when no matching increment occurred yields unpredictable results: debug execution panics; release execution produces a large value that the active-check reads as nonzero."

**N3** — "Debug builds" and "release builds" are compilation-profile terms. The contract can state the two observable behaviors without naming the build profiles. Suggested: "Two execution modes exist: one panics on underflow; the other wraps around to a large value."

**N4** — "Independently usable flow references" is near the boundary. The consumer-facing contract is that concurrent callers receive handles that do not block each other. This is acceptable if rephrased to avoid "references" (which connotes `Arc`).

---

## Spec-to-Source Comparison

### Purpose
| Claim | Source Match | Status |
|-------|-------------|--------|
| Authoritative source of scheduling-entity state | `DashMap<FlowId, Arc<Flow>>` stores all flows | ✅ Matches |
| Each flow identity maps to exactly one registered entry | DashMap key uniqueness | ✅ Matches |
| Concurrent access to independently usable flow references | `Arc<Flow>` returned from `get_or_create` | ✅ Matches |
| Does not manage scheduling policy | No scheduling logic present | ✅ Matches |
| Maintains per-flow attributes that scheduling policy reads | `Flow` fields expose weight, priority, depth, credit, etc. | ✅ Matches |

### Non-goals
| Claim | Source Match | Status |
|-------|-------------|--------|
| Not a queue; no ordering among waiting flows | No ordering stored; snapshot order is caller-supplied | ✅ Matches |
| Does not validate weight or priority ranges | No range checks in `register` or `set_weight`/`set_priority` | ✅ Matches |
| Does not enforce scheduling policy | No scheduling logic | ✅ Matches |
| Flows cannot be removed | No `remove`, `drop`, or `unregister` method | ✅ Matches |
| Active-count provides no underflow protection | `fetch_sub` on `AtomicU32` — no saturation | ✅ Matches |

### Interface — Construction
| Claim | Source Match | Status |
|-------|-------------|--------|
| Instantiated with default weight and priority | `FlowRegistry::new(default_weight: f64, default_priority: u32)` | ✅ Matches |
| Defaults apply only to lookup-created flows | `get_or_create` uses `self.default_weight`/`default_priority`; `register` uses payload values | ✅ Matches |
| Flow constructible independently of registry | `Flow::new(id: FlowId, default_weight: f64, default_priority: u32)` exists | ✅ Matches |

### Interface — Registration Payload
| Claim | Source Match | Status |
|-------|-------------|--------|
| `FlowRegistration` is a public data type with public fields: identity, weight, priority | `pub struct FlowRegistration { pub id: FlowId, pub weight: f64, pub priority: u32 }` | ✅ Matches |

### Interface — Registration
| Claim | Source Match | Status |
|-------|-------------|--------|
| Creates new entry or updates existing weight/priority | `register()` branches: `get_mut` updates, else inserts | ✅ Matches |
| Always succeeds; reports whether insertion occurred | Returns `bool` (`true` = created, `false` = updated) | ✅ Matches |
| Uses payload weight/priority, not registry defaults | `reg.weight` and `reg.priority` stored directly | ✅ Matches |

### Interface — Lookup
| Claim | Source Match | Status |
|-------|-------------|--------|
| Returns independently usable shared flow reference | Returns `Arc<Flow>` | ✅ Matches |
| Creates with defaults if not registered | `or_insert_with` creates `Flow::new(flow_id, dw, dp)` | ✅ Matches |
| Concurrent first-time lookups yield single entry | `DashMap::entry()` API is atomic check-and-insert | ✅ Matches |

### Interface — Aggregate Queries
| Claim | Source Match | Status |
|-------|-------------|--------|
| Count of registered flows | `len() -> usize` | ✅ Matches |
| Whether empty | `is_empty() -> bool` | ✅ Matches |
| Sum of per-flow depth counters as u32 | `sum_depths() -> u32` via `.sum()` on iterator of u32 loads | ✅ Matches |
| Reads live state without modifying registry | `iter()` + `load(Ordering::Relaxed)` is read-only | ✅ Matches |

### Interface — Queue Snapshots
| Claim | Source Match | Status |
|-------|-------------|--------|
| `QueueSnapshot` has public fields: active (u64), waiting (u64), flows (Vec<QueueFlowEntry>) | `pub struct QueueSnapshot { pub active: u64, pub waiting: u64, pub flows: Vec<QueueFlowEntry> }` | ✅ Matches |
| `QueueFlowEntry` has public fields: id (String), position (u64, 1-based) | `pub struct QueueFlowEntry { pub id: String, pub position: u64 }` | ✅ Matches |
| Caller supplies global counts and ordered list of identities | `queue_snapshot(active: u64, waiting: u64, wait_order: I)` where `I: IntoIterator<Item = FlowId>` | ✅ Matches |
| Filters to registered flows with positive depth | `self.flows.get(&flow_id)` followed by `depth.load() > 0` | ✅ Matches |
| Deduplicates entries | `HashSet<String>` called `seen` prevents duplicate identities | ✅ Matches |
| Assigns contiguous 1-based positions preserving caller order | `position` starts at 1, increments after each inclusion | ✅ Matches |
| Discards unknown or zero-depth identities | Skipped when `get()` returns None or depth <= 0 | ✅ Matches |

### Interface — Per-Flow Attributes
| Claim | Source Match | Status |
|-------|-------------|--------|
| Weight readable/writable via dedicated methods | `weight() -> f64`, `set_weight(w: f64)` | ✅ Matches |
| Priority readable/writable via dedicated methods | `priority() -> u32`, `set_priority(p: u32)` | ✅ Matches |
| Depth directly accessible as public field | `pub depth: AtomicU32` | ✅ Matches |
| Credit directly accessible as public field | `pub credit: AtomicI64` | ✅ Matches |
| Enqueued timestamp directly accessible as public field | `pub enqueued_at: RwLock<Option<Instant>>` | ✅ Matches |
| Active count directly accessible as public field | `pub active: AtomicU32` | ✅ Matches |
| Active supports increment and decrement operations | `inc_active()`, `dec_active()` | ✅ Matches |
| Flow considered active when in-flight count is nonzero | `is_active()` returns `active.load() > 0` | ✅ Matches |
| **`id` field publicly accessible** | `pub id: FlowId` on `Flow` | ⚠️ **Undocumented** — see Findings F1 |

### Interface — Flow Identity
| Claim | Source Match | Status |
|-------|-------------|--------|
| Opaque dedicated type, not interchangeable with raw string | `struct FlowId(String)` is distinct type | ✅ Matches |
| Construction from string | `FlowId::new(id: impl Into<String>)` | ✅ Matches |
| Equality by underlying string value | Derives `PartialEq`, `Eq`; inner `String` comparison is value-based | ✅ Matches |
| Display outputs identity string | `Display` writes `self.0` | ✅ Matches |
| Debug wraps with type prefix | `Debug` writes `"FlowId({})"` | ✅ Matches |
| Ephemeral when string starts with `"ephemeral-"` | `is_ephemeral()` checks `starts_with("ephemeral-")` | ✅ Matches |
| Ephemeral metric label resolves to single common value | `metric_label()` returns `"ephemeral"` for ephemeral flows | ✅ Matches |
| Named metric label equals identity string | `metric_label()` returns `&self.0` for named flows | ✅ Matches |

### Invariants
| Invariant | Source Match | Status |
|-----------|-------------|--------|
| Each flow identity maps to at most one entry; creation paths never produce duplicates | DashMap key uniqueness; `entry().or_insert_with()` prevents races | ✅ Matches |
| Flows remain registered for lifetime of registry; no unregistration | No removal API exists | ✅ Matches |
| Weight, priority, credit, depth updated individually; no cross-attribute atomicity | Separate atomics; no transaction or batch update | ✅ Matches |
| Snapshot lists only positive-depth flows, no duplicates, contiguous 1-based positions | Verified above | ✅ Matches |
| Ephemeral metric label resolves to single value; named resolves to identity string | Verified above | ✅ Matches |

### Constraints
| Constraint | Source Match | Status |
|------------|-------------|--------|
| Weight/priority updates not mutually exclusive with concurrent reads | Separate atomics with `Relaxed` ordering | ✅ Matches |
| Active decrement underflow: debug panics, release wraps to max | `AtomicU32::fetch_sub(1)` — checked_sub panics in debug, wrapping_sub in release | ✅ Matches |
| Wrap-around max value interpreted as nonzero (active) | `is_active()` returns `load() > 0`; u32::MAX > 0 | ✅ Matches |
| Depth sum may overflow 32-bit | `.sum() -> u32` uses standard overflow rules | ✅ Matches |
| Snapshot positions reflect relative order; not absolute queue index | Sequential counter; skipped IDs renumber later entries | ✅ Matches |
| Global counts are caller-supplied; not cross-checked | `active` and `waiting` stored directly without validation | ✅ Matches |

---

## Findings

### F1 — Undocumented Behavior: `Flow.id` is a public field
**Classification:** `undocumented_behavior`

The `Flow` struct exposes `pub id: FlowId` as a directly accessible public field. The spec's "Per-Flow Attributes" section lists weight, priority, credit, depth, enqueued timestamp, and active as per-flow attributes but omits `id` as a publicly accessible field on the Flow type. The "Flow Identity" section describes the `FlowId` type independently but does not document that a consumer can read the identity from a `Flow` instance via field access. While `FlowId::new()` and the registry operations already expose identity semantics, the direct accessibility of `Flow.id` is an interface surface not reflected in the spec.

**Severity:** Low — the identity is accessible via the registry's lookup (which returns the FlowId as a key) and via `FlowId::to_string()`. The `Flow.id` field is a convenient accessor, not a critical contract gap.

---

### F2 — Missing Interface: `FlowId` derives `Hash`
**Classification:** `missing_interface`

The spec describes `FlowId` as supporting "equality by underlying string value" but does not document that `FlowId` derives `Hash`. This is relevant because it enables `FlowId` to be used as a key in hash-based collections (e.g., `HashMap<FlowId, _>`, `HashSet<FlowId>`), which is an externally usable property. The spec's "Flow Identity" section should mention hash support if consumers might place `FlowId` in collections.

**Severity:** Low — this is a minor surface. The type is used as a DashMap key internally, and external consumers who want to build their own collections benefit from knowing Hash is available.

---

### F3 — No How Violation: Implementation Terminology in Per-Flow Attributes
**Classification:** `spec_error`

The Per-Flow Attributes section contains the phrase "their own atomic or locked mutation surfaces." This is implementation-specific terminology that leaks data structure details. The contract should state that each counter attribute is independently mutable and safe for concurrent access without specifying the synchronization mechanism.

**Severity:** Medium — violates the "No How" constraint.

---

### F4 — No How Violation: Implementation Terminology in Constraints
**Classification:** `spec_error`

The Constraints section states "The active-count decrement uses unsaturated subtraction on an unsigned counter: in debug builds... in release builds..." The phrases "unsaturated subtraction," "unsigned counter," "debug builds," and "release builds" are implementation-specific. The observable behavior (panic vs. wrap-around) should be stated as the contract without referencing the underlying mechanism or compilation profile names.

**Severity:** Medium — violates the "No How" constraint.

---

### F5 — Minor Spec Inaccuracy: `FlowId::new()` Accepts `impl Into<String>`
**Classification:** `spec_error`

The spec states the identity "supports identity construction from a string." The actual signature is `FlowId::new(id: impl Into<String>)`, which accepts any type convertible to `String` (e.g., `&str`, `Cow<str>`, `String`). The spec's wording is technically correct but undersells the interface. Not a bug, but the spec should reflect the broader acceptor if it wants to be precise.

**Severity:** Low — the current description is not incorrect, only less informative than it could be.

---

## Summary

| Classification | Count |
|---------------|-------|
| `spec_error` (No How violations) | 3 (F3, F4, F5) |
| `undocumented_behavior` | 1 (F1) |
| `missing_interface` | 1 (F2) |
| `bug` | 0 |

The spec is **substantially correct** in its contractual claims. All behavioral assertions about the registry, flows, identity, snapshots, and aggregate queries match the source code. The primary issues are:

1. **Two "No How" violations** (F3, F4) where implementation terminology leaks into the spec. These require rewording to describe observable behavior rather than mechanisms.
2. **One undocumented public field** (F1) — `Flow.id` is accessible but not listed in the Per-Flow Attributes section.
3. **One minor missing interface** (F2) — `Hash` derive on `FlowId` is not documented.
4. **One underspecified constructor** (F5) — `FlowId::new()` accepts a broader type than "from a string."

No bugs were found in the implementation that contradict the spec. The spec describes behavior that the code faithfully implements.
