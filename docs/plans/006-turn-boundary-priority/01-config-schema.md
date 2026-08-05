# 01 — Config Schema

**Parent:** `PLAN.md`

## Objective

Replace the median-gap config fields with turn-boundary state-machine
fields. This is the foundation: the cadence rewrite (02) and all
downstream tasks read the new `PriorityPolicy`.

## Files

| File | Change |
|---|---|
| `src/config/mod.rs` | Replace 4 fields on `PriorityPolicy` with 3 new ones; update `Default` impl; update default fns |
| `src/config/loader.rs` | Replace 4 `set_default` calls (lines 107-110) with 3 new ones; replace validation block (lines 296-306) |
| `tests/policy_config.rs` | Update tests that reference removed fields; add tests for new fields and validation |
| `tests/config.rs` | Update tests that reference removed fields |

## Steps

### 1. `src/config/mod.rs` — `PriorityPolicy` struct (line ~295)

Replace the field set:

```rust
// REMOVE these fields:
pub interactive_gap_min: Duration,
pub background_gap_max: Duration,
pub sample_window: usize,
pub min_samples: usize,

// ADD these fields:
pub idle_gap_threshold: Duration,
pub agentic_suspected_threshold: u32,
pub agentic_confirmed_threshold: u32,
```

Keep `enabled: bool` unchanged.

### 2. `src/config/mod.rs` — default fns (lines ~319-333)

Remove `default_interactive_gap_min`, `default_background_gap_max`,
`default_sample_window`, `default_min_samples`.

Add:

```rust
fn default_idle_gap_threshold() -> Duration {
    Duration::from_secs(30)
}

fn default_agentic_suspected_threshold() -> u32 {
    5
}

fn default_agentic_confirmed_threshold() -> u32 {
    12
}
```

### 3. `src/config/mod.rs` — `serde` attributes

Each new field needs a `#[serde(default = "PriorityPolicy::default_...")]`
attribute with the `humantime_serde` wrapper for the `Duration` field
(match the existing `interactive_gap_min` pattern):

```rust
#[serde(
    default = "PriorityPolicy::default_idle_gap_threshold",
    with = "loader::humantime_serde"
)]
pub idle_gap_threshold: Duration,

#[serde(default = "PriorityPolicy::default_agentic_suspected_threshold")]
pub agentic_suspected_threshold: u32,

#[serde(default = "PriorityPolicy::default_agentic_confirmed_threshold")]
pub agentic_confirmed_threshold: u32,
```

### 4. `src/config/mod.rs` — `Default` impl (line ~336)

Replace the body:

```rust
impl Default for PriorityPolicy {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            idle_gap_threshold: Self::default_idle_gap_threshold(),
            agentic_suspected_threshold: Self::default_agentic_suspected_threshold(),
            agentic_confirmed_threshold: Self::default_agentic_confirmed_threshold(),
        }
    }
}
```

### 5. `src/config/loader.rs` — defaults (lines 107-110)

Replace:

```rust
.set_default("priority_policy.interactive_gap_min", "30s")?
.set_default("priority_policy.background_gap_max", "2s")?
.set_default("priority_policy.sample_window", 20u64)?
.set_default("priority_policy.min_samples", 3u64)?
```

With:

```rust
.set_default("priority_policy.idle_gap_threshold", "30s")?
.set_default("priority_policy.agentic_suspected_threshold", 5u32)?
.set_default("priority_policy.agentic_confirmed_threshold", 12u32)?
```

### 6. `src/config/loader.rs` — validation (lines 296-306)

Replace the `interactive_gap_min`/`background_gap_max`/`sample_window`/
`min_samples` validation with:

```rust
let pp = &cfg.priority_policy;
if pp.agentic_confirmed_threshold <= pp.agentic_suspected_threshold {
    return Err(anyhow::anyhow!(
        "priority_policy.agentic_confirmed_threshold must be strictly greater than agentic_suspected_threshold"
    ));
}
if pp.agentic_suspected_threshold == 0 {
    return Err(anyhow::anyhow!(
        "priority_policy.agentic_suspected_threshold must be >= 1"
    ));
}
```

### 7. Update tests

`tests/policy_config.rs` — replace references to
`interactive_gap_min`/`background_gap_max`/`sample_window`/`min_samples`
with the new fields. The "should error when X <= Y" test becomes
"agentic_confirmed_threshold must be > agentic_suspected_threshold".

`tests/config.rs` — update any assertions on the old fields; the default
`PriorityPolicy` now has `idle_gap_threshold: 30s`,
`agentic_suspected_threshold: 5`, `agentic_confirmed_threshold: 12`.

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all --test policy_config --test config
```

Confirm:
- `PriorityPolicy::default()` produces the new fields with the right
  defaults.
- A config YAML with the old keys (`interactive_gap_min`, etc.) loads
  without error (unknown fields ignored).
- A config YAML with the new keys overrides the defaults.
- Validation rejects `agentic_confirmed_threshold <=
  agentic_suspected_threshold`.
- Validation rejects `agentic_suspected_threshold == 0`.
