# c_sched_priority — Spec

## Purpose

This concept selects a single flow from a set of eligible candidates using a deterministic three-level ordering. Priority is the dominant criterion, with base preference and arrival time providing tiebreaks. An empty candidate set produces no winner (not an error), and the selected flow is always a member of the input candidate set.

- Higher priority always wins over lower priority, unconditionally.
- Among equal priority, base preference score determines the winner; scores within a tolerance band are treated as equal.
- Among equal priority and equivalent base score, earliest enqueued flow wins.
- The winner is always one of the provided candidates, never a synthesized identity.

## Non-goals

This concept does not manage priorities, admit flows, or distribute load.

- Does not assign, modify, or derive priority values.
- Does not enforce admission gates or rate limits.
- Does not balance work across executors or partitions.
- Does not handle preemption or rescinding of selections.
- Does not guarantee that the selected flow can be executed.

## Interface

The selection contract accepts a set of candidates, each carrying a priority level, a base preference score, and a monotonic arrival time, and returns one winner identity or indicates no candidate was supplied.

### Selection Contract

- Inputs: a set of candidates (possibly empty), each with a priority value (unsigned, larger = more urgent), a real-valued base preference score (lower = preferred), and a monotonically increasing enqueue timestamp from a single clock domain.
- Output: the identity of the single winning candidate, or absent when the candidate set is empty.
- No error conditions beyond the empty-set case; the selection surface does not propagate failures.
- The winner is always one of the provided candidates, never a synthesized identity.
- Comparison is deterministic: the same inputs always produce the same winner.
- Candidates with non-finite base scores (not-a-number or infinity) are excluded from winning; they are silently bypassed as if absent.

## Invariants

The selection ordering preserves a strict three-level hierarchy that cannot be violated by any rewrite. The equivalence check for base scores takes precedence over the "lower score wins" rule — scores within a tolerance band trigger the FIFO tiebreak, not the lower-score preference.

- **Priority dominance:** A candidate with a strictly higher priority always wins regardless of base score or enqueue time.
- **Base-score direction (strict difference):** When priorities are equal and base scores differ by more than the tolerance threshold, the candidate with the lower base score wins.
- **Base-score equivalence:** Two base scores are treated as equivalent when their absolute difference does not exceed a small positive tolerance value. Equivalent scores do not trigger the "lower wins" rule.
- **FIFO tiebreak:** When both priority and base score are equivalent, the candidate with the earlier enqueue time wins.
- **Output membership:** The returned identity corresponds to exactly one element from the input set.

## Constraints

The ordering and selection logic is bounded by a fixed hierarchy depth and the properties of real-valued scores compared with tolerance.

- The ordering has exactly three levels; no additional tiebreakers are guaranteed beyond enqueue time.
- Base scores are real-valued; callers must supply finite scores (not not-a-number and not infinity). Non-finite scores are silently excluded from consideration rather than rejected.
- Priority is an unsigned scalar; larger values indicate higher urgency.
- Enqueue timestamps originate from a monotonic clock within a single process lifetime; they are not comparable across process restarts or distributed processes.
- Among candidates identical on all three ordering fields, any one may be selected; the specific choice is not part of the contract.

## Rationale

The three-level hierarchy balances urgency, algorithmic preference, and fairness.

- Priority dominates because urgent work must preempt lower-priority flows regardless of other metrics.
- Base score provides a secondary preference signal from the base scheduling algorithm without overriding explicit priority.
- Enqueue-time tiebreak preserves fairness: among equally deserving candidates, the earliest arrival goes first.
- Tolerance-based score equivalence prevents spurious ordering differences from numerical noise, and equivalence is checked before the "lower wins" rule to avoid bypassing the FIFO tiebreak.
- Empty-set absence (rather than error) reflects that no candidates is a valid, non-fault state.

## Related

- `[[?c_flow]]` — flows submitted to the scheduler
- `[[?c_base_scheduler]]` — the base scheduling algorithm that produces base preference scores
- `[[src/scheduler/priority.rs#FlowCandidate]]` — candidate identity, priority, score, and timestamp
- `[[src/scheduler/priority.rs#select_best]]` — selection entry point
