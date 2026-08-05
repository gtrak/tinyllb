//! Unit tests for the turn-boundary cadence state machine (Plan 006, Task 05).
//!
//! Exercises the classify state machine via `CadenceRegistry`, covering:
//! cold start, continuous tool demotion, idle-chunk promotion,
//! fast-turn-boundary counter reset, override blocking, disabled policy,
//! and the key burst-then-idle pattern that the old median model misclassified.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tinyllb::config::{Priorities, PriorityPolicy};
use tinyllb::flow::cadence::CadenceRegistry;
use tinyllb::flow::{Flow, FlowId};

/// Helper: build a `CadenceRegistry` with the default test policy.
fn make_registry() -> CadenceRegistry {
    let policy = PriorityPolicy {
        enabled: true,
        idle_gap_threshold: Duration::from_secs(30),
        agentic_suspected_threshold: 5,
        agentic_confirmed_threshold: 12,
    };
    let classes = Priorities {
        interactive: 100,
        agent: 50,
        background: 10,
    };
    CadenceRegistry::new(Arc::new(policy), Arc::new(classes))
}

/// Helper: build a test `Flow` with default weight and priority.
fn make_flow(id: &str, default_priority: u32) -> Flow {
    Flow::new(FlowId::new(id), 1.0, default_priority)
}

/// Record `count` arrivals with uniform `gap`, all as non-turn-boundary (role:tool).
fn record_tool_arrivals(
    registry: &CadenceRegistry,
    flow_id: &FlowId,
    gap: Duration,
    count: usize,
) {
    let t0 = Instant::now();
    for i in 0..count {
        registry.record_arrival(flow_id, t0 + gap * i as u32, false);
    }
}

/// Record `count` arrivals with uniform `gap`, all as turn-boundary (role:user).
fn record_user_arrivals(
    registry: &CadenceRegistry,
    flow_id: &FlowId,
    gap: Duration,
    count: usize,
) {
    let t0 = Instant::now();
    for i in 0..count {
        registry.record_arrival(flow_id, t0 + gap * i as u32, true);
    }
}

/// Record a single arrival at a specific time with an explicit turn-boundary flag.
fn record_at(registry: &CadenceRegistry, flow_id: &FlowId, t: Instant, is_turn_boundary: bool) {
    registry.record_arrival(flow_id, t, is_turn_boundary);
}

// ---------------------------------------------------------------------------
// Cold start tests
// ---------------------------------------------------------------------------

/// Test: a single user arrival on a brand-new flow stays Cold (priority 100).
#[test]
fn cold_start_is_interactive() {
    let reg = make_registry();
    let flow = make_flow("cold", 100);
    let id = FlowId::new("cold");

    record_at(&reg, &id, Instant::now(), true);

    reg.classify_and_apply(&flow, &id);
    assert_eq!(flow.priority(), 100, "Cold start should be interactive (100)");
}

/// Test: 3 user arrivals with 10s gaps (< 30s threshold) stay Cold (priority 100).
#[test]
fn cold_stays_interactive_under_threshold() {
    let reg = make_registry();
    let flow = make_flow("cold-fast-turns", 100);
    let id = FlowId::new("cold-fast-turns");

    // 3 user arrivals with 10s gaps (all < 30s idle_gap_threshold).
    // Fast turn boundaries reset the counter but don't promote to Interactive.
    // State stays Cold → priority 100.
    record_user_arrivals(&reg, &id, Duration::from_secs(10), 3);

    reg.classify_and_apply(&flow, &id);
    assert_eq!(flow.priority(), 100, "Fast turn boundaries should stay Cold/Interactive (100)");
}

// ---------------------------------------------------------------------------
// Continuous tool demotion tests
// ---------------------------------------------------------------------------

/// Test: 5 tool arrivals demote from Cold to AgenticSuspected (priority 50).
#[test]
fn continuous_tool_demotes_to_suspected() {
    let reg = make_registry();
    let flow = make_flow("suspected", 100);
    let id = FlowId::new("suspected");

    // 5 continuous tool arrivals → counter hits 5 → AgenticSuspected.
    record_tool_arrivals(&reg, &id, Duration::from_secs(1), 5);

    reg.classify_and_apply(&flow, &id);
    assert_eq!(flow.priority(), 50, "5 tool arrivals should demote to AgenticSuspected (50)");
}

/// Test: 12 tool arrivals demote from Cold to AgenticConfirmed (priority 10).
#[test]
fn continuous_tool_demotes_to_confirmed() {
    let reg = make_registry();
    let flow = make_flow("confirmed", 100);
    let id = FlowId::new("confirmed");

    // 12 continuous tool arrivals → counter hits 12 → AgenticConfirmed.
    record_tool_arrivals(&reg, &id, Duration::from_secs(1), 12);

    reg.classify_and_apply(&flow, &id);
    assert_eq!(flow.priority(), 10, "12 tool arrivals should demote to AgenticConfirmed (10)");
}

// ---------------------------------------------------------------------------
// Idle chunk promotion tests
// ---------------------------------------------------------------------------

/// Test: an idle chunk (45s gap, role:user) promotes from AgenticSuspected to Interactive (100).
#[test]
fn idle_chunk_promotes_to_interactive() {
    let reg = make_registry();
    let flow = make_flow("promote", 100);
    let id = FlowId::new("promote");

    // 5 tool arrivals → AgenticSuspected (priority 50).
    let t0 = Instant::now();
    for i in 0..5 {
        record_at(&reg, &id, t0 + Duration::from_secs(i as u64), false);
    }
    reg.classify_and_apply(&flow, &id);
    assert_eq!(flow.priority(), 50, "Should be AgenticSuspected (50)");

    // User arrival with 45s gap → idle chunk → Interactive.
    record_at(&reg, &id, t0 + Duration::from_secs(45), true);
    reg.classify_and_apply(&flow, &id);
    assert_eq!(flow.priority(), 100, "Idle chunk should promote to Interactive (100)");
}

/// Test: a 45s gap before a role:tool arrival does NOT promote (not a turn boundary).
#[test]
fn tool_gap_does_not_promote() {
    let reg = make_registry();
    let flow = make_flow("tool-gap", 100);
    let id = FlowId::new("tool-gap");

    // 5 tool arrivals → AgenticSuspected (priority 50).
    let t0 = Instant::now();
    for i in 0..5 {
        record_at(&reg, &id, t0 + Duration::from_secs(i as u64), false);
    }
    reg.classify_and_apply(&flow, &id);
    assert_eq!(flow.priority(), 50, "Should be AgenticSuspected (50)");

    // Tool arrival with 45s gap → NOT a turn boundary, stays AgenticSuspected.
    record_at(&reg, &id, t0 + Duration::from_secs(45), false);
    reg.classify_and_apply(&flow, &id);
    assert_eq!(flow.priority(), 50, "Tool gap should NOT promote (still 50)");
}

// ---------------------------------------------------------------------------
// Fast turn boundary counter reset test
// ---------------------------------------------------------------------------

/// Test: fast turn boundary resets counter, preventing threshold crossing.
#[test]
fn fast_turn_boundary_resets_counter() {
    let reg = make_registry();
    let flow = make_flow("counter-reset", 100);
    let id = FlowId::new("counter-reset");

    // 4 tool arrivals → counter = 4 (just under threshold 5).
    let t0 = Instant::now();
    for i in 0..4 {
        record_at(&reg, &id, t0 + Duration::from_secs(i as u64), false);
    }

    // User arrival with 5s gap (fast turn boundary, gap < 30s).
    // Resets counter to 0. State stays Cold.
    record_at(&reg, &id, t0 + Duration::from_secs(5), true);

    // 4 more tool arrivals → counter = 4 again (never hit threshold 5).
    for i in 0..4 {
        record_at(&reg, &id, t0 + Duration::from_secs(6 + i as u64), false);
    }

    reg.classify_and_apply(&flow, &id);
    assert_eq!(flow.priority(), 100, "Counter reset should keep Cold (100)");
}

// ---------------------------------------------------------------------------
// Demotion from Cold/Interactive after continuous run
// ---------------------------------------------------------------------------

/// Test: Cold flow demotes to AgenticSuspected after 5 continuous tool arrivals.
#[test]
fn interactive_demotes_after_continuous_run() {
    let reg = make_registry();
    let flow = make_flow("demote", 100);
    let id = FlowId::new("demote");

    // 1 user arrival → Cold (priority 100).
    let t0 = Instant::now();
    record_at(&reg, &id, t0, true);

    // 5 tool arrivals with 1s gaps → counter hits 5 → AgenticSuspected.
    for i in 0..5 {
        record_at(&reg, &id, t0 + Duration::from_secs(1 + i as u64), false);
    }

    reg.classify_and_apply(&flow, &id);
    assert_eq!(flow.priority(), 50, "5 tool arrivals should demote to AgenticSuspected (50)");
}

// ---------------------------------------------------------------------------
// State cycling test
// ---------------------------------------------------------------------------

/// Test: flow cycles Interactive → AgenticConfirmed → Interactive.
#[test]
fn interactive_cycles_back() {
    let reg = make_registry();
    let flow = make_flow("cycle", 100);
    let id = FlowId::new("cycle");

    let t0 = Instant::now();

    // First arrival (tool, Cold)
    record_at(&reg, &id, t0, false);

    // User arrival with 45s gap → idle chunk → Interactive
    record_at(&reg, &id, t0 + Duration::from_secs(45), true);
    reg.classify_and_apply(&flow, &id);
    assert_eq!(flow.priority(), 100, "Should be Interactive (100)");

    // 12 tool arrivals → AgenticSuspected at 5, AgenticConfirmed at 12
    for i in 0..12 {
        record_at(&reg, &id, t0 + Duration::from_secs(46 + i as u64), false);
    }
    reg.classify_and_apply(&flow, &id);
    assert_eq!(flow.priority(), 10, "Should be AgenticConfirmed (10)");

    // User arrival with 45s gap → idle chunk → Interactive
    record_at(&reg, &id, t0 + Duration::from_secs(46 + 12 + 45), true);
    reg.classify_and_apply(&flow, &id);
    assert_eq!(flow.priority(), 100, "Should cycle back to Interactive (100)");
}

// ---------------------------------------------------------------------------
// Override blocking test
// ---------------------------------------------------------------------------

/// Test: explicit priority override (source=1) blocks heuristic classification.
#[test]
fn header_override_blocks_heuristic() {
    let reg = make_registry();
    let flow = make_flow("override", 50);
    let id = FlowId::new("override");

    // Set priority source to header (1) — blocks heuristic writes.
    flow.set_priority_source(1);

    // 12 tool arrivals would normally demote to AgenticConfirmed (priority 10).
    record_tool_arrivals(&reg, &id, Duration::from_secs(1), 12);

    reg.classify_and_apply(&flow, &id);
    assert_eq!(flow.priority(), 50, "Header override should block heuristic (still 50)");
}

// ---------------------------------------------------------------------------
// Disabled policy test
// ---------------------------------------------------------------------------

/// Test: disabled policy leaves priority unchanged regardless of arrivals.
#[test]
fn disabled_heuristic_no_change() {
    let policy = PriorityPolicy {
        enabled: false,
        idle_gap_threshold: Duration::from_secs(30),
        agentic_suspected_threshold: 5,
        agentic_confirmed_threshold: 12,
    };
    let classes = Priorities {
        interactive: 100,
        agent: 50,
        background: 10,
    };
    let reg = CadenceRegistry::new(Arc::new(policy), Arc::new(classes));
    let flow = make_flow("disabled", 50);
    let id = FlowId::new("disabled");

    // User arrivals with 60s gaps would normally promote to Interactive (100).
    record_user_arrivals(&reg, &id, Duration::from_secs(60), 5);

    reg.classify_and_apply(&flow, &id);
    assert_eq!(flow.priority(), 50, "Disabled policy should keep default (50)");
}

// ---------------------------------------------------------------------------
// Burst-then-idle pattern test (the key pattern the median model got wrong)
// ---------------------------------------------------------------------------

/// Test: the `[1,1,1,45,1,1,1,60]` pattern — burst then idle — is correctly
/// classified as Interactive (100). The old median model would classify this
/// as background (10) because median(gaps) = 1s.
#[test]
fn burst_then_idle_pattern() {
    let reg = make_registry();
    let flow = make_flow("burst-idle", 100);
    let id = FlowId::new("burst-idle");

    let t0 = Instant::now();
    record_at(&reg, &id, t0, true);                                    // user, first arrival
    record_at(&reg, &id, t0 + Duration::from_secs(1), false);         // tool, gap 1s
    record_at(&reg, &id, t0 + Duration::from_secs(2), false);         // tool, gap 1s
    record_at(&reg, &id, t0 + Duration::from_secs(3), false);         // tool, gap 1s
    record_at(&reg, &id, t0 + Duration::from_secs(48), true);         // user, gap 45s -> idle chunk!
    record_at(&reg, &id, t0 + Duration::from_secs(49), false);        // tool, gap 1s
    record_at(&reg, &id, t0 + Duration::from_secs(50), false);        // tool, gap 1s
    record_at(&reg, &id, t0 + Duration::from_secs(51), false);        // tool, gap 1s
    record_at(&reg, &id, t0 + Duration::from_secs(111), true);        // user, gap 60s -> idle chunk!

    reg.classify_and_apply(&flow, &id);
    assert_eq!(flow.priority(), 100, "Two idle chunks should be Interactive (100)");
}
