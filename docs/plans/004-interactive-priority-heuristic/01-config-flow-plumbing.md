# 01 — Config + Flow/FlowRegistry plumbing

**Phase:** 1 (foundation)
**Depends on:** none.
**Blocks:** `02`, `03`, `04`, `05`.

## Objective

Add the `PriorityPolicy` config struct, extend `Flow` with an explicit
priority-override field, and wire `CadenceRegistry` into
`FlowRegistry`. This is pure plumbing — no classification logic yet
(that's `02`) and no header parsing (that's `03`).

## Files

| File | Change |
| --- | --- |
| `src/config/mod.rs` | Add `PriorityPolicy` struct + `priority_policy` field on `Config`. |
| `src/config/loader.rs` | Add defaults for `priority_policy.*` in `load()`. |
| `src/flow/mod.rs` | Add `priority_override: AtomicU8` to `Flow`; add `CadenceRegistry` field on `FlowRegistry`; accessor methods. |
| `src/flow/cadence.rs` | NEW stub: `Cadence`, `CadenceRegistry` (empty capsule — logic lands in 02). |

## Steps

1. Define `PriorityPolicy` in `src/config/mod.rs`:

   ```rust
   #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
   pub struct PriorityPolicy {
       #[serde(default = "PriorityPolicy::default_enabled")]
       pub enabled: bool,
       #[serde(default, with = "loader::humantime_serde")]
       pub interactive_gap_min: Duration,   // default 30s
       #[serde(default, with = "loader::humantime_serde")]
       pub background_gap_max: Duration,    // default 2s
       #[serde(default = "PriorityPolicy::default_sample_window")]
       pub sample_window: usize,            // default 20
       #[serde(default = "PriorityPolicy::default_min_samples")]
       pub min_samples: usize,              // default 3
   }
   ```

   Follow the existing `Backpressure` / `KvPolicyConfig` pattern —
   implement `Default`, store as `pub priority_policy: PriorityPolicy`
   on `Config` with `#[serde(default)]`.

2. Add to `src/config/loader.rs` `load()`:

   ```text
   .set_default("priority_policy.enabled", true)?
   .set_default("priority_policy.interactive_gap_min", "30s")?
   .set_default("priority_policy.background_gap_max", "2s")?
   .set_default("priority_policy.sample_window", 20u64)?
   .set_default("priority_policy.min_samples", 3u64)?
   ```

   Add a `validate` check: `interactive_gap_min > background_gap_max`,
   and `sample_window >= min_samples`.

3. Extend `Flow` in `src/flow/mod.rs` with:

   ```rust
   /// Source of the current `priority` value.
   /// 0 = heuristic-derived (default).
   /// 1 = explicitly pinned via X-LLM-Priority header.
   /// 2 = explicitly pinned via POST /flows admin API.
   pub priority_source: AtomicU8,
   ```

   Add `priority_source()` / `set_priority_source(u8)` accessors
   mirroring the existing `priority`/`set_priority` pattern.
   Initialize to `0` in `Flow::new` and in `FlowRegistry::register`
   (the admin path sets it to `2`).

4. `FlowRegistry::new` gains a `priority_policy: PriorityPolicy`
   parameter and stores it. Update the two existing call sites
   (`src/scheduler/mod.rs::Scheduler::new` and `new_with_defaults`)
   to thread it through. (`new_with_defaults` uses
   `PriorityPolicy::default()`.)

5. Create `src/flow/cadence.rs` with an empty module scaffold so the
   `FlowRegistry` can hold an `Arc<CadenceRegistry>`:

   ```rust
   pub struct CadenceRegistry {
       inner: DashMap<FlowId, Cadence>,
   }
   pub struct Cadence {
       arrivals: VecDeque<Instant>,
   }
   impl CadenceRegistry {
       pub fn new() -> Self { /* empty map */ }
   }
   ```

   Add `pub mod cadence;` to `src/flow/mod.rs`. The classify method
   lands in 02. For now `CadenceRegistry::new()` is the only public
   API consumed here.

6. Add `cadence: Arc<CadenceRegistry>` to `FlowRegistry`. Construct it
   in `FlowRegistry::new`. Add a public accessor
   `registry.cadence() -> &Arc<CadenceRegistry>` so the scheduler (in
   04) and any future admin endpoint can reach it.

## Verification

- `cargo build --all-targets` clean.
- `cargo test --all` green (no behavior change yet).
- Existing `flow_identify` tests still pass.
- New unit test: `priority_policy_default_is_enabled()` asserting the
  bool defaults to `true`.
- New unit test: `validate_rejects_inverted_gaps()` in
  `tests/policy_config.rs` passing.

