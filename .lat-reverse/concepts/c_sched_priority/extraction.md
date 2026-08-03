# c_sched_priority — Extraction

## Responsibilities

- Select a single flow from a candidate set using priority as the dominant ordering criterion.

## Interface Surfaces

### `FlowCandidate` (exported struct, line 15–26)

- Represents one eligible flow presented to the selection algorithm.
- **Fields (inputs):** `flow_id` (identity), `priority` (u32, higher = more urgent), `enqueued_at` (Instant), `base_score` (f64, lower = preferred by base algorithm).

### `select_best` (exported function, line 36–65)

- **Inputs:** `&[FlowCandidate]` — a slice of candidate references.
- **Outputs:** `Option<FlowId>` — the winning flow identity, or `None` when the candidate set is empty.
- **Selection order (deterministic, three-level sort):**
  1. Highest `priority` wins (line 46).
  2. Ties broken by lowest `base_score` (line 50).
  3. Further ties broken by earliest `enqueued_at` (line 54).
- **Error contract:** No `Result`; empty input yields `None`. No other error path.

## Invariants

- **Priority dominance:** A higher `priority` value always wins regardless of `base_score` or `enqueued_at` (line 46).
- **Base-score preference direction:** Among equal priorities, the lower `base_score` wins (line 50).
- **FIFO tie-break:** Among equal priority and base-score, the earlier `enqueued_at` wins (line 54).
- **Epsilon equivalence:** Two `base_score` values are considered equal when their absolute difference is below `f64::EPSILON` (line 52).
- **Output membership:** The returned `FlowId` is always the `flow_id` of some element from the input slice (line 64).

## Failure Modes

- **Empty candidate set:** Returns `None` (lines 37, 64).
- **NaN `base_score`:** Floating-point comparison with `NaN` yields `false` on all ordering predicates; the `NaN` candidate will never become the best choice, but is silently skipped (lines 46–54).
- **Identical candidates:** When all three fields are equal across candidates, the first candidate in iteration order wins (lines 48, 52, 54 — all comparisons are strict `<`/`>`, never `<=`/`>=`).

## Code Evidence

- `src/scheduler/priority.rs:15-26` — `FlowCandidate` struct definition.
- `src/scheduler/priority.rs:36-65` — `select_best` function and comparison logic.
- `src/scheduler/priority.rs:46` — Priority comparison (`>`).
- `src/scheduler/priority.rs:50` — Base-score comparison (`<`).
- `src/scheduler/priority.rs:52` — Epsilon threshold for float equality.
- `src/scheduler/priority.rs:54` — Enqueue-time comparison (`<`).
