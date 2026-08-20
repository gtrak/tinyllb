# 02 — KV gate instant-rejects in blocking mode; reject path duplicated across 3 sites; config splits one subsystem into two keys

Status: **RESOLVED & DEPLOYED (2026-08-20)** · Component: `tinyllb` (scheduler admission, config)
Reported: 2026-08-20 · Severity: high (worker threads die under transient KV pressure)

## Resolution Summary

The worker-death bug and its structural root cause are fixed, reviewed,
committed, and deployed to the live tinyllb service on 2026-08-20.

### What changed

**Change 1 — KV reject band is now mode-aware (the worker-death fix).**
`KvPolicy::check` now absorbs `Reject` into `Delay` for `BackpressureMode::Blocking`,
so `kv_usage > reject_threshold` HOLDS (unbounded, matching the DRR blocking
contract and the delay band) instead of instant-429ing. A transient >0.95 spike
is ridden out rather than killing the thread. Hybrid/FailFast keep the instant
429, now computed via `fail_fast_retry_after` (the same formula the DRR scheduler
and the delay-timeout path use). `KVMDecision::Reject(Duration)` was reduced to a
bare `Reject` variant, DELETING the ad-hoc `5.0 + excess * 10.0` retry_after
formula that lived in `decide()`. The stall gate is unchanged (instant 429 in all
modes for an unrecoverable wedge — correct).

**Change 3 — `kv_policy` nested under `backpressure` (structural root-cause fix).**
The config split one admission subsystem into two sibling keys (`backpressure:`
and `kv_policy:`), which is why the KV reject band was implemented without a mode.
`KvPolicyConfig` is now a nested field on `Backpressure`, so the gate inherits the
hold-vs-reject contract from `backpressure.mode` by construction. A deprecated
`Option<KvPolicyConfig>` migration sentinel on `Config` makes a stale top-level
`kv_policy:` key fail loudly (with a message naming `backpressure` + `kv_policy`)
instead of being silently ignored. `KvPolicy::new` / `Scheduler::new` signatures are
unchanged (the fix is at the config level only).

### Change 2 — reject-path unification: folded into Change 1
The issue proposed a `reject_for(mode, reason)` helper routing all reject sites.
The substantive unification (KV hybrid/failfast retry_after now uses the shared
`fail_fast_retry_after`, deleting the ad-hoc formula) is achieved by Change 1.
The explicit `reject_for` enum/helper was NOT added as a separate abstraction:
the remaining reject sites the issue lists are intentional edges — the stall gate
(fixed 5s, all modes, unrecoverable) and the DRR oneshot-drop (fixed 1s, can fire
in blocking mode when a sender is dropped — a catastrophic edge, not transient
pressure, so a mode-aware helper with a `mode != Blocking` assertion would panic
there). Routing those through a mode-aware helper would add risk to working code
for no behavioral gain. The formula unification + config nesting address the root
cause with less risk. Flagged as a deliberate deviation from the issue's Option A.

### Deviation from the issue's safety analysis
The issue claimed that in blocking mode a wedged KV would be caught by
`request_timeout` (300s) yielding a 408. That is **inaccurate**: `request_timeout`
wraps `builder.send()` (the backend forward), not `admit_with_turn_boundary` (the KV
gate hold). So the blocking KV hold is unbounded until KV drops or the client
disconnects; a genuinely wedged engine is caught first by the stall gate (instant
429). The `lat.md/admission.md` invariant was written to reflect actual behavior,
not the 408 claim.

### Tests
- `tests/kv_admission.rs`: +1 test `kv_admission_blocking_holds_reject_band`
  (blocking holds at 0.96, admits when KV drops — the exact worker-death scenario);
  3 existing reject-band tests switched to Hybrid (where instant-429 still holds);
  reject-test retry_after assertion relaxed to non-zero. 14/14 green.
- `tests/gateway.rs`: the issue-01 pressure test switched Blocking→Hybrid (it
  encoded the pre-fix blocking+0.96→429 behavior that now holds). 11/11 green.
- `tests/config.rs`: +1 test `top_level_kv_policy_errors_with_migration_message`.
- 18 `Backpressure` struct literals across phase1/phase2/phase3/backpressure tests
  gained `kv_policy: Default::default()`.
- `cargo test --all` green (0 failures).

### Docs
`lat.md/admission.md` (mode-aware reject invariant + Reject variant),
`lat.md/scheduler.md` (blocking contract applies to all admission gates, stall
gate excepted), `lat.md/config.md` (nesting + migration), `config.example.yaml`.

### Commits
- `8f5f4aa` fix(scheduler): KV gate holds (not 429) in blocking mode at reject band
- `550aef7` refactor(config): nest kv_policy under backpressure

### Deployment (2026-08-20)
`cargo build --release` → `~/.local/bin/tinyllb`; moved the `kv_policy:` block under
`backpressure:` in the live `~/.config/tinyllb/config.yaml` (removed the top-level
key); `systemctl --user restart tinyllb`. Verified: config loaded with
`backpressure.kv_policy { enabled: true, reject_threshold: 0.95, delay_threshold:
0.8, bypass_interactive: true }` and the migration sentinel `kv_policy: None` (no
top-level key → no migration error); service active; `GET /v1/models` → 200.

---

*Original report below.*



## Summary

In `backpressure.mode: blocking` (the production config), the KV-cache
admission gate **instantly 429s** any request when `kv_usage > reject_threshold`
(0.95), bypassing the blocking contract that every other admission path honors
("wait indefinitely, never proactively reject"). This killed two worker
threads on 2026-08-20 ~15:47–15:50 UTC.

The root cause is threefold:

1. **The KV reject band is mode-blind.** `KvPolicy::decide()` returns
   `Reject` for `kv_usage > 0.95` in *all* backpressure modes, including
   `blocking`. The `Delay` band (0.80–0.95) correctly honors blocking mode
   (unbounded wait), but the `Reject` band does not — it short-circuits to an
   instant `Err` regardless of mode. Meanwhile the DRR scheduler in blocking
   mode *never* proactively rejects (it blocks on a oneshot forever); only the
   oneshot-drop edge produces a reject. So the KV gate is the one admission
   path that violates the blocking contract under load.

2. **`BackpressureRejected` is constructed ad-hoc in 3+ separate sites** with
   different `retry_after` formulas and no shared mode-awareness. The reject
   semantics are duplicated rather than unified, which is how the KV gate ended
   up mode-blind while the DRR scheduler is mode-aware.

3. **The config splits one admission subsystem into two unrelated top-level
   keys.** `backpressure:` and `kv_policy:` are siblings in `Config`
   (config/mod.rs:21 and :33), not nested. `KvPolicyConfig` carries only
   `enabled`/`reject_threshold`/`delay_threshold` — it has **no** `mode`,
   `max_wait`, `max_queue_depth`, or `retry_after_base` fields; those live on
   `Backpressure` and are threaded into `KvPolicy` at construction time
   (kv_admission.rs:91-112). The config never told the KV gate it was supposed
   to honor a mode, so the reject band was implemented without one. This is
   the structural reason the two paths diverged: they were configured,
   defaulted, and documented as independent policies.

This report covers the live evidence (two dead worker threads), the three
reject sites and their divergent semantics, the config split, the proposed
resolution (unify the reject path and the config; make the KV gate defer to
the backpressure mode like the DRR scheduler does), and test/deployment
impact.

## Evidence (live system, 2026-08-20 ~15:47–15:50 UTC)

### Dead workers

Two `worker` subagent sessions died with `APIError Too Many Requests: queue
full`:

| Session | Slug | Died (UTC) | Last error |
|---------|------|------------|------------|
| `ses_fe02840f2ffe4Rdat8PwLR5Zpr` | stellar-wolf | 15:47:36 | `APIError` 429 "queue full" |
| `ses_fe027a641ffeW2NrzMRHKosRlV` | quiet-nebula | 15:50:39 | `APIError` 429 "queue full" |

The stored error (opencode DB, message `msg_01fdb3d7b001EqacQnT1IXBJy5`):

```json
{
  "name": "APIError",
  "data": {
    "message": "Too Many Requests: queue full",
    "statusCode": 429,
    "isRetryable": true,
    "responseHeaders": {
      "content-length": "22",
      "content-type": "application/json",
      "date": "Thu, 20 Aug 2026 15:48:07 GMT",
      "retry-after": "6"
    },
    "responseBody": "{\"error\":\"queue full\"}"
  }
}
```

Note: `retry-after: 6` **is present** (disproving the initial hypothesis that
the KV 429 lacked the header). The value `6` matches the KV reject formula
`5.0 + excess * 10.0` (kv_admission.rs:123) at `kv_usage ≈ 0.98` (excess=0.03 →
5.3s → ceil → 6). The 429 came from tinyllb, not the backend.

### KV pressure was transient, not a wedge

vLLM engine logs (`journalctl --user -u vllm`) over 15:44–15:52 show GPU KV
cache usage oscillating, not stuck:

```
15:50:03  KV cache usage: 50.0%   Running: 1  Waiting: 2
15:50:33  KV cache usage: 87.7%   Running: 2  Waiting: 0
15:50:43  KV cache usage: 86.2%   Running: 2  Waiting: 0
15:50:53  KV cache usage: 54.6%   Running: 1  Waiting: 0
15:51:13  KV cache usage: 86.9%   Running: 2  Waiting: 0
...
15:54:53  KV cache usage: 94.6%   Running: 2  Waiting: 0
```

KV spiked above 0.95 on ~30–60s timescales as agentic contexts churned, then
dropped back. The backend was **not** stalled (`llm_backend_stalled 0`; no
`backend inference stall detected` events in this window). The earlier stalls
(13:56, 14:27, 14:28) had cleared; the engine was healthy and serving.

### The KV gate fired 42 instant rejections

From `curl localhost:1234/metrics`:

```
llm_kv_admission_decisions_total{decision="accept"}  287
llm_kv_admission_decisions_total{decision="delay"}   128
llm_kv_admission_decisions_total{decision="reject"}   42
llm_backpressure_rejections_total{mode="blocking"}    42
```

The `reject` count (42) exactly equals `backpressure_rejections_total`. In
blocking mode, the DRR scheduler never proactively rejects — so **all 42
429s came from the KV gate** (the stall gate was not active in this window).

### The same workers' earlier requests were held and admitted

tinyllb admit logs show the dead workers' *previous* requests were held in the
scheduler (DRR + KV delay band) for 50–99s and successfully admitted:

```
15:44:38  ses_fe027a641ffe  admit accept  wait_seconds=10.4   priority=100
15:45:22  ses_fe027a641ffe  admit accept  wait_seconds=29.2   priority=100
15:46:42  ses_fe027a641ffe  admit accept  wait_seconds=66.5   priority=100
15:47:02  ses_fe02840f2ffe  admit accept  wait_seconds=74.8   priority=100
15:48:04  ses_fe027a641ffe  admit accept  wait_seconds=62.4   priority=100
15:49:19  ses_fe027a641ffe  admit accept  wait_seconds=54.6   priority=100
15:51:45  ses_fe027a641ffe  admit accept  wait_seconds=59.8   priority=50
```

These held because `kv_usage` was in the delay band (0.80–0.95) at admit
time. The fatal requests hit a momentary `kv_usage > 0.95` snapshot → instant
429 → retry loop → exhausted.

### Timeline of one death (stellar-wolf)

- 15:47:02 — previous request admitted (wait 74.8s).
- 15:47:36 — assistant message row created (`time.created`).
- ~15:47:36 — next request hits KV gate, `kv_usage > 0.95` → instant 429
  (`retry-after: 6`). opencode retries: 2 executor retries (≤10s each) + up to
  5 session retries (6s each, honoring `retry-after`).
- 15:48:07 — final 429 response (`date` header). ~31s elapsed — consistent
  with ~5 retry attempts × 6s.
- Error stored, thread dies.

The 31s retry window was spent re-snapshooting into momentary `>0.95` windows.
KV dropped below 0.95 repeatedly during that window (vLLM logs show 86%↔54%
oscillation) — a hold would have ridden it out.

## Root cause 1 — KV reject band is mode-blind

Call chain (production, `backpressure.mode: blocking`):

```
proxy_handler                              src/gateway/proxy.rs:360
  └─ scheduler.admit_with_turn_boundary    src/scheduler/mod.rs:202
       ├─ kv_policy.check().await?         src/scheduler/mod.rs:240
       │    └─ KvPolicy::check             src/scheduler/kv_admission.rs:150
       │         decide() (line 118):
       │           kv_usage > 0.95 → Reject → instant Err (ALL modes)   ← BUG
       │           kv_usage > 0.80 → Delay  → blocking: unbounded wait  ← correct
       ├─ stall gate (line 246)            instant Err if stalled       ← justified
       └─ inner.admit (line 253)           DRR: blocking = block forever ← correct
```

`decide()` (kv_admission.rs:118-130) takes a single snapshot and branches with
**no awareness of `backpressure_mode`**:

```rust
fn decide(&self, snapshot: &BackendSnapshot) -> KVMDecision {
    if snapshot.kv_usage > self.reject_threshold {
        let excess = snapshot.kv_usage - self.reject_threshold;
        let retry_after = Duration::from_secs_f64(5.0 + excess * 10.0);
        KVMDecision::Reject(retry_after)          // ← instant, mode-blind
    } else if snapshot.kv_usage > self.delay_threshold {
        KVMDecision::Delay                        // ← check() honors mode here
    } else {
        KVMDecision::Accept
    }
}
```

`check()` (line 150-226) only consults `self.backpressure_mode` in the `Delay`
branch (line 185-211): blocking = unbounded wait, hybrid/failfast = bounded by
`max_wait` then reject. The `Reject` branch (line 218-224) returns `Err`
immediately in all modes:

```rust
KVMDecision::Reject(retry_after) => {
    self.metrics.kv_admission_decisions_total
        .with_label_values(&["reject"]).inc();
    Err(BackpressureRejected { retry_after })   // ← no mode check
}
```

This contradicts the documented blocking contract (line 147):
> **Blocking**: waits indefinitely (blocking contract).

The contract holds for the delay band and the DRR scheduler, but silently
breaks at the reject threshold.

### Why this kills workers

In blocking mode, the DRR scheduler holds forever (drr.rs:510-574, blocks on a
oneshot — only fails if the sender drops). The KV delay band holds forever.
But the KV reject band instant-429s. So a worker whose request arrives during
a momentary `kv_usage > 0.95` spike gets a 429 instead of being held — even
though:

- The same worker's previous request was held 74.8s and admitted fine.
- KV dropped below 0.95 within tens of seconds.
- The DRR scheduler would have held the request indefinitely.

The 429 starts opencode's retry clock (2 executor + 5 session retries, ~30–40s
budget at `retry-after: 6`). Each retry re-snapshots into a fresh momentary
spike. If the spikes outlast the retry budget (~30s), the thread dies
permanently. A hold would have cost ~30–60s of latency and survived.

### Contrast: the stall gate is justified

The stall gate (mod.rs:246-251) also instant-429s in all modes:

```rust
if *self.stall_rx.borrow() {
    tracing::info!("admit rejected: backend stalled");
    return Err(BackpressureRejected {
        retry_after: Duration::from_secs(5),
    });
}
```

This is correct: a stalled engine is genuinely wedged and cannot be waited out
(the stall watchdog will abort in-flight streams after 30s). Holding would just
accumulate requests that all get aborted. KV pressure above 0.95 is **not** a
wedge — it's transient load that the delay band exists to absorb.

## Root cause 2 — reject path duplicated across 3 sites

`BackpressureRejected { retry_after }` is constructed independently in 3+
locations with different `retry_after` formulas and no shared mode-awareness:

| Site | File:line | Mode-aware? | retry_after formula |
|------|-----------|-------------|---------------------|
| KV gate reject | `kv_admission.rs:223` | **No** | `5.0 + excess * 10.0` |
| Stall gate | `mod.rs:248` | No (intentional) | fixed `5s` |
| DRR oneshot-drop | `drr.rs:562` | N/A (edge) | fixed `1s` |
| DRR failfast depth | `drr.rs:586` | Yes (failfast only) | `fail_fast_retry_after(depth, max_qd, base)` |
| DRR hybrid timeout | `drr.rs:671` | Yes (hybrid only) | `fail_fast_retry_after(depth, max_qd, base)` |
| DRR hybrid oneshot-drop | `drr.rs:663` | N/A (edge) | fixed `1s` |

Each site independently decides whether and how to reject. There is no single
admission-decision type or helper that enforces: "in blocking mode, prefer hold
over reject; only reject for unrecoverable conditions (stall)." This
duplication is how the KV gate ended up mode-blind while the DRR scheduler is
mode-aware — they were written separately and never unified.

The `BackpressureRejected` type itself (backpressure.rs) is just a struct with
a `retry_after` field and a `fail_fast_retry_after` helper. There is no
mode-aware `reject()` constructor or `AdmissionDecision` enum that all gates
funnel through.

## Root cause 3 — config splits one subsystem into two top-level keys

The config layout is the structural origin of the divergence. In `Config`
(config/mod.rs:12-40), admission is governed by **two sibling keys**:

```rust
pub struct Config {
    pub backend: Backend,
    pub scheduler: Scheduler,
    pub flows: Flows,
    pub priorities: Priorities,
    pub backpressure: Backpressure,     // ← line 21: mode, max_queue_depth, max_wait, retry_after_base
    pub metrics: Metrics,
    pub server: Server,
    pub request_timeout: Option<Duration>,
    pub kv_policy: KvPolicyConfig,      // ← line 33: enabled, reject_threshold, delay_threshold
    pub retry_policy: RetryPolicy,
    pub priority_policy: PriorityPolicy,
}
```

`Backpressure` (mod.rs:418-433):

```rust
pub struct Backpressure {
    pub mode: BackpressureMode,              // blocking | failfast | hybrid
    pub max_queue_depth: u32,
    pub max_wait: Duration,
    pub retry_after_base: Duration,
}
```

`KvPolicyConfig` (mod.rs:85-92):

```rust
pub struct KvPolicyConfig {
    pub enabled: bool,
    pub reject_threshold: f64,
    pub delay_threshold: f64,
}
```

`KvPolicyConfig` has **no mode field** — and no `max_wait`, `max_queue_depth`,
or `retry_after_base`. Those four values live exclusively on `Backpressure`.
`KvPolicy` only learns about them at construction (kv_admission.rs:91-112),
where `new()` takes them as separate parameters threaded from the scheduler's
`Backpressure` config:

```rust
pub fn new(
    config: &KvPolicyConfig,
    monitor: Arc<BackendMonitor>,
    metrics: Arc<Metrics>,
    backpressure_mode: BackpressureMode,   // ← threaded in, not in kv_policy config
    max_wait: Duration,                     // ← threaded in
    retry_after_base: Duration,             // ← threaded in
    max_queue_depth: u32,                   // ← threaded in
) -> Self
```

The YAML mirrors this — two independent blocks with no nesting:

```yaml
backpressure:
  mode: blocking
  max_queue_depth: 100
  max_wait: 10s

kv_policy:
  enabled: true
  reject_threshold: 0.95
  delay_threshold: 0.80
```

And loader.rs sets their defaults in separate groups (line 78 for
`backpressure.*`, lines 83-84 for `kv_policy.*`).

**Why this caused the bug.** The KV gate is conceptually part of the
backpressure/admission subsystem — it's a pre-admission gate that decides
hold-vs-reject under pressure, exactly like the DRR scheduler's backpressure
paths. But the config presents it as an independent policy with its own
thresholds and no mode concept. The operator sets `mode: blocking` under
`backpressure:` and reasonably expects *all* admission gates to honor it; the
`kv_policy:` block gives no signal that it's exempt. The code only bolted
mode-awareness onto the KV *delay* band (kv_admission.rs:185-211) as an
afterthought — the *reject* band was never updated, because nothing in the
config structure suggested the KV gate owed the mode any allegiance.

The fix should collapse these into a single config block so the KV gate is
structurally a sub-policy of backpressure, inheriting `mode` (and the
hold-vs-reject contract) by construction rather than by parameter threading.

## Proposed resolution

The guiding principle: **the KV gate should reject like anything else** —
i.e., defer to the backpressure mode contract, the same way the DRR scheduler
does. In blocking mode that means hold, not instant-429. And **the reject code
path should be unified**, not duplicated.

### Change 1 — collapse KV reject band into delay band for blocking mode

`src/scheduler/kv_admission.rs`

In `check()`, the `Reject` branch should respect `backpressure_mode`:

- **Blocking mode:** treat `kv_usage > reject_threshold` the same as `Delay` —
  wait (unbounded) for `kv_usage <= delay_threshold`. Do not return `Err`. The
  upper bound is `request_timeout` (proxy.rs, 300s) — if KV never drops, the
  request times out with a 408, which is the correct blocking-mode failure
  (timeout, not 429).
- **Hybrid / FailFast:** keep the instant reject (those modes are explicitly
  "fail fast"). Use the existing `fail_fast_retry_after` formula for
  consistency with the DRR scheduler, replacing the ad-hoc
  `5.0 + excess * 10.0`.

Concretely, `decide()` stays as-is (it's a pure pressure classification), but
`check()`'s `Reject` arm becomes mode-aware — mirroring the existing
`Delay` arm's mode switch (line 185-211). In blocking mode, `Reject` falls
through to the same `wait_for(|s| s.kv_usage <= self.delay_threshold)` loop.

This eliminates the `kv_admission_decisions_total{decision="reject"}` counter
in blocking mode entirely (reject becomes impossible; the worst case is a long
delay). The counter still increments in hybrid/failfast.

### Change 2 — unify the reject path

Introduce a single mode-aware rejection helper that all admission gates use,
replacing the ad-hoc `BackpressureRejected { retry_after: <whatever> }`
constructions.

Option A (minimal): add a method on `BackpressureRejected` (or a free fn in
`backpressure.rs`):

```rust
/// Construct a rejection that respects the backpressure mode.
/// In blocking mode this is only called for unrecoverable conditions
/// (stall); transient pressure should hold, not reject.
pub fn reject_for(mode: BackpressureMode, reason: RejectReason) -> BackpressureRejected {
    match reason {
        RejectReason::Stall => BackpressureRejected { retry_after: Duration::from_secs(5) },
        RejectReason::QueueFull { depth, max_depth, base } => {
            assert!(mode != BackpressureMode::Blocking); // blocking never queue-rejects
            BackpressureRejected { retry_after: fail_fast_retry_after(depth, max_depth, base) }
        }
        RejectReason::KvPressure { depth, max_depth, base } => {
            assert!(mode != BackpressureMode::Blocking); // blocking holds instead
            BackpressureRejected { retry_after: fail_fast_retry_after(depth, max_depth, base) }
        }
    }
}
```

Option B (deeper): introduce an `AdmissionDecision` enum
(`Accept | Hold(until) | Reject(reason)`) returned by all gates, with a single
mode-aware `resolve()` that converts `Hold` → wait or `Reject` based on mode.
This is more invasive but makes the mode contract unmissable.

**Recommendation:** Option A for the immediate fix (unblocks the worker-death
bug); track Option B as a follow-up if more gates are added.

All three reject sites (KV gate, stall gate, DRR) are updated to route through
the unified helper. The stall gate keeps its instant-429 in all modes (it
passes `RejectReason::Stall` which ignores mode). The KV gate in blocking mode
no longer calls the helper at all (it holds). The DRR failfast/hybrid paths
use `RejectReason::QueueFull`.

### Change 3 — nest `kv_policy` under `backpressure` in config

`src/config/mod.rs`, `src/config/loader.rs`, `~/.config/tinyllb/config.yaml`

Collapse the two sibling keys into one so the KV gate is structurally a
sub-policy of backpressure. `KvPolicyConfig` becomes a nested struct on
`Backpressure`, inheriting `mode` (and the hold-vs-reject contract) by
construction instead of by parameter threading.

New `Backpressure`:

```rust
pub struct Backpressure {
    pub mode: BackpressureMode,
    pub max_queue_depth: u32,
    pub max_wait: Duration,
    pub retry_after_base: Duration,
    #[serde(default)]
    pub kv_policy: KvPolicyConfig,   // ← now nested
}
```

New YAML:

```yaml
backpressure:
  mode: blocking
  max_queue_depth: 100
  max_wait: 10s
  kv_policy:                  # ← nested, no longer a sibling
    enabled: true
    reject_threshold: 0.95
    delay_threshold: 0.80
```

Consequences:

- `KvPolicy::new()` no longer takes `backpressure_mode`, `max_wait`,
  `retry_after_base`, `max_queue_depth` as separate parameters — it reads them
  from the parent `Backpressure` struct (or the scheduler holds a single
  `Backpressure` reference and passes the relevant fields). This removes the
  parameter-threading at kv_admission.rs:91-112 and makes it impossible to
  construct a `KvPolicy` without a mode.
- loader.rs: merge the `kv_policy.*` defaults (lines 83-84) into the
  `backpressure.*` default group (line 78), i.e.
  `backpressure.kv_policy.reject_threshold` / `backpressure.kv_policy.delay_threshold`.
- The validation in loader.rs:158-170 (`delay_threshold < reject_threshold`)
  moves under the backpressure validation block — same check, just relocated.

**Backward compatibility.** The live config (`~/.config/tinyllb/config.yaml`)
and any other deployments use the flat `kv_policy:` top-level key. Two
options:

- **Recommended (breaking, clean):** drop support for the top-level
  `kv_policy:` key. Update the live config in the same commit. This is an
  internal tool with one deployment; the migration is a one-time YAML edit.
  Add a config-load error with a helpful message if a top-level `kv_policy:`
  key is present ("`kv_policy` has moved under `backpressure`; nest it there").
- **Soft migration:** accept both `backpressure.kv_policy` and a top-level
  `kv_policy:` (deprecated) during a transition window, with the nested form
  taking precedence and a warn log on the deprecated form. Remove after one
  release cycle.

Given single-deployment scope, the clean break is simpler and removes the
"two independent policies" ambiguity that caused this bug.

### What does NOT change

- **Stall gate** (mod.rs:246-251): unchanged. Instant 429 in all modes for a
  stalled engine is correct.
- **DRR blocking mode** (drr.rs:510-574): unchanged. Already holds forever.
- **KV delay band** (0.80–0.95): unchanged. Already holds in blocking mode.
- **KV reject band in hybrid/failfast**: behavior unchanged (instant 429), but
  retry_after formula switches from `5.0 + excess * 10.0` to
  `fail_fast_retry_after` for consistency.
- **`request_timeout: 300s`** (proxy.rs): unchanged. This is the upper bound
  that catches a request held too long in the KV delay loop. A 408 timeout
  after 300s is the correct blocking-mode failure for unrecoverable KV
  pressure — far better than an instant 429 that kills the thread in 30s.

## Safety / edge-case analysis

- **Hard KV saturation (wedged at 99%).** If KV is genuinely stuck above 0.95
  (not oscillating), blocking mode now holds the request until
  `request_timeout` (300s) → 408. This is worse latency than an instant 429,
  but: (a) a wedged KV is usually a stall, which the stall gate catches first
  (instant 429); (b) 300s timeout is opencode-retryable (408 matches
  `/timeout/` in RETRYABLE_MESSAGE_PATTERNS); (c) the thread survives a 408
  retry, unlike a 429-exhaustion death. Net: strictly better for thread
  survival.

- **Queue depth growth during hold.** Held KV requests count in
  `delayed_count` (visible in `queue_depth()`). In blocking mode the DRR
  already holds unboundedly, so this is not a new failure mode — it's the
  existing contract. The `max_queue_depth` check is failfast-only and does not
  apply in blocking mode.

- **retry_after formula change (hybrid/failfast).** Switching from
  `5.0 + excess * 10.0` to `fail_fast_retry_after(depth, max_qd, base)` changes
  the header value in hybrid/failfast modes. `fail_fast_retry_after` =
  `base * (1 + depth / max_qd)`. With `retry_after_base: 1s` (default) and
  moderate depth, this yields smaller values (1–2s) vs the old 5–15s. This is
  fine — hybrid/failfast are supposed to fail fast with short retry-after.
  Verify the hybrid/failfast KV tests still pass (they check for 429, not a
  specific retry-after value).

- **Metrics cardinality.** `kv_admission_decisions_total{decision="reject"}`
  no longer increments in blocking mode. Existing dashboards/alerts on that
  counter in blocking deployments will flatline. This is correct (rejects
  should not happen in blocking mode) but should be noted in the changelog.
  The `delay` counter will increase instead.

- **opencode retry budget.** opencode has 2 executor retries (≤10s) + 5
  session retries (honoring `retry-after`). With the fix, the KV gate no longer
  429s in blocking mode, so this budget is only consumed by stall-gate 429s
  (genuinely unrecoverable) and backend 5xx — which is the intended scope.

## Test impact

### `tests/kv_admission.rs` (must change)

Existing reject-band tests assume instant 429 at `kv_usage > reject_threshold`
in all modes. With the fix, blocking mode no longer rejects — it delays.

- Tests using `BackpressureMode::Blocking` + `kv_usage > reject_threshold`:
  change assertion from "returns 429" to "delays (holds) until kv_usage drops
  below delay_threshold, then proceeds." Use a `BackendMonitor` that publishes
  a high-then-low snapshot sequence and assert the request eventually admits.
- Tests using `BackpressureMode::Hybrid` / `FailFast` + `kv_usage >
  reject_threshold`: keep instant-429 assertion, but update the expected
  `retry_after` if the test asserts a specific value (switch to
  `fail_fast_retry_after` formula). Most tests likely only check
  `is_err()`/status, not the exact duration — verify.
- Add a new test: blocking mode, `kv_usage` oscillates above/below
  `reject_threshold` — assert the request is held through the spike and
  admitted when KV drops (the exact scenario that killed the workers).

### `tests/scheduler_drr.rs`, `tests/priority_live.rs` (likely unaffected)

These use the DRR scheduler directly (empty KV monitor → accept). The KV gate
is not exercised. Verify no behavioral change.

### `tests/gateway.rs` (verify)

Any test that saturates KV to 0.96 in blocking mode and expects a 429 will
break. Search for `reject_threshold` / `0.95` / `queue full` in gateway tests.
If found, update to expect a delay/timeout instead, or switch the test to
hybrid/failfast mode where instant-429 is still correct.

### New unified-reject tests

If Option A is taken: add unit tests for `BackpressureRejected::reject_for`
covering all `RejectReason` × `BackpressureMode` combinations, asserting:
- `Stall` → 429 in all modes.
- `QueueFull` / `KvPressure` → 429 in hybrid/failfast, panic/debug-assert in
  blocking (should never be called).

### Config-nesting tests

- `tests/config.rs` (or equivalent): add a test that loads a YAML with
  `backpressure.kv_policy` nested and asserts the fields parse. Add a test
  that the deprecated top-level `kv_policy:` key (if soft-migration is taken)
  still parses with a warn, or (if clean-break) that it errors with the
  helpful message.
- Update any test helper that builds a `Config` with `kv_policy` to use the
  nested path.

## Docs to update

- `lat.md/admission.md` — "KV-Cache-Aware Admission Gate": add an invariant —
  *in blocking mode, the reject threshold does not produce an instant 429;
  requests are held in the delay loop until KV pressure drops or
  `request_timeout` elapses. Instant KV-gate 429s only occur in hybrid/failfast
  modes.* Document the unified reject helper.
- `lat.md/scheduler.md` — "Backpressure modes": clarify that the blocking
  contract ("wait indefinitely, never proactively reject") applies to **all**
  admission gates (KV gate, DRR scheduler), with the sole exception of the
  stall gate (unrecoverable wedge).
- `lat.md/config.md` — document the new `backpressure.kv_policy` nesting and
  that `mode` is inherited by the KV gate (no separate mode knob). Note the
  migration from the top-level `kv_policy:` key.
- `~/.config/tinyllb/config.yaml` — **must change**: move the `kv_policy:`
  block under `backpressure:`. Add a comment noting that `reject_threshold`
  only causes instant 429 in hybrid/failfast modes; in blocking mode it
  extends the delay band.
- `~/opt/vllm/WORKLOG.md` — log the incident, root cause, and fix per
  AGENTS.md.

## Deployment

1. `cargo build --release` in `~/dev/vllm-frontend`; copy
   `target/release/tinyllb` → `~/.local/bin/tinyllb`.
2. **Config change required**: in `~/.config/tinyllb/config.yaml`, move the
   `kv_policy:` block to nest under `backpressure:`:
   ```yaml
   backpressure:
     mode: blocking
     max_queue_depth: 100
     max_wait: 10s
     kv_policy:
       enabled: true
       reject_threshold: 0.95
       delay_threshold: 0.80
   ```
   Remove the top-level `kv_policy:` key.
3. `systemctl --user restart tinyllb.service`.
4. Verify:
   - Config loads without error (no "top-level kv_policy" warning/error).
   - Under agent load that spikes KV > 0.95: `curl -s localhost:1234/metrics |
     grep kv_admission_decisions` shows `reject` no longer incrementing (stays
     at 0 in blocking mode); `delay` increments instead.
   - `llm_backpressure_rejections_total{mode="blocking"}` only increments on
     stall-gate events (check: `journalctl --user -u tinyllb | grep 'admit
     rejected: backend stalled'`), not on KV pressure.
   - Worker threads survive KV spikes: their requests show `admit accept` with
     long `wait_seconds` (held through the spike) instead of dying with 429.
   - If KV is genuinely wedged (>0.95 sustained): requests time out at
     `request_timeout` (300s) with 408, not instant 429. Confirm a 408 is
     retryable by opencode (matches `/timeout/` pattern).

## Out of scope

- Issue 01 (interactive KV bypass / metadata passthrough) — orthogonal; both
  fixes can coexist. Issue 01 adds a bypass for priority-100 flows; this issue
  fixes the reject semantics for all flows in blocking mode.
- The earlier `neon-wolf` death (14:28) — that was a `ContextOverflowError`
  (400), not a KV-gate 429. Separate cause (context compression disabled;
  transcript grew past 180k tokens).
- opencode's retry budget (2 executor + 5 session retries) — adequate once
  tinyllb stops 429ing on transient pressure. Not changing.
- The vLLM-side KV-offload deadlock (#46453) — the stalls at 13:56/14:27/14:28
  were that bug; the stall gate + watchdog handle it. This issue is about the
  *non-stall* KV-pressure case.
