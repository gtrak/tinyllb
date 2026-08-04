//! Unit tests for the cadence classification heuristic (Plan 004, Task 02).
//!
//! Exercises the classify table at boundary gaps, hysteresis guard,
//! and priority-override blocking behavior.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tinyllb::config::{Priorities, PriorityPolicy};
use tinyllb::flow::cadence::CadenceRegistry;
use tinyllb::flow::{Flow, FlowId};

/// Helper: build a `CadenceRegistry` with the default test policy.
fn make_registry() -> CadenceRegistry {
    let policy = PriorityPolicy {
        enabled: true,
        interactive_gap_min: Duration::from_secs(30),
        background_gap_max: Duration::from_secs(2),
        sample_window: 20,
        min_samples: 3,
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

/// Helper: record arrivals with uniform gaps from a fixed base time.
fn record_uniform_arrivals(
    registry: &CadenceRegistry,
    flow_id: &FlowId,
    gap: Duration,
    count: usize,
) {
    let t0 = Instant::now();
    for i in 0..count {
        registry.record_arrival(flow_id, t0 + gap * i as u32);
    }
}

// ---------------------------------------------------------------------------
// classify() direct tests (via CadenceRegistry)
// ---------------------------------------------------------------------------

/// Test: cold start (< min_samples) leaves priority unchanged.
#[test]
fn cold_start_keeps_default() {
    let registry = make_registry();
    let flow = make_flow("cold", 50);
    let flow_id = FlowId::new("cold");

    // Record only 2 arrivals (min_samples = 3), classify should do nothing.
    let t0 = Instant::now();
    registry.record_arrival(&flow_id, t0);
    registry.record_arrival(&flow_id, t0 + Duration::from_secs(1));

    registry.classify_and_apply(&flow, &flow_id);
    assert_eq!(
        flow.priority(),
        50,
        "Cold start should keep default priority 50"
    );
}

/// Test: 5 arrivals at 0.5s gaps → median = 500ms ≤ 2s → background.
#[test]
fn rapid_fire_demotes_to_background() {
    let registry = make_registry();
    let flow = make_flow("rapid", 50);
    let flow_id = FlowId::new("rapid");

    record_uniform_arrivals(&registry, &flow_id, Duration::from_millis(500), 5);

    registry.classify_and_apply(&flow, &flow_id);
    assert_eq!(
        flow.priority(),
        10,
        "Rapid-fire flow should be demoted to background (10)"
    );
}

/// Test: 5 arrivals at 60s gaps → median = 60s ≥ 30s → interactive.
#[test]
fn slow_paced_promotes_to_interactive() {
    let registry = make_registry();
    let flow = make_flow("slow", 50);
    let flow_id = FlowId::new("slow");

    record_uniform_arrivals(&registry, &flow_id, Duration::from_secs(60), 5);

    registry.classify_and_apply(&flow, &flow_id);
    assert_eq!(
        flow.priority(),
        100,
        "Slow-paced flow should be promoted to interactive (100)"
    );
}

/// Test: 5 arrivals at 10s gaps → median = 10s (between 2s and 30s) → agent.
#[test]
fn medium_keeps_agent() {
    let registry = make_registry();
    let flow = make_flow("medium", 50);
    let flow_id = FlowId::new("medium");

    record_uniform_arrivals(&registry, &flow_id, Duration::from_secs(10), 5);

    registry.classify_and_apply(&flow, &flow_id);
    assert_eq!(
        flow.priority(),
        50,
        "Medium-paced flow should stay at agent priority (50)"
    );
}

// ---------------------------------------------------------------------------
// Boundary tests
// ---------------------------------------------------------------------------

/// Test: gap exactly == background_gap_max (2s) → background.
#[test]
fn boundary_exact_background_gap_max() {
    let registry = make_registry();
    let flow = make_flow("boundary-bg", 50);
    let flow_id = FlowId::new("boundary-bg");

    // 5 arrivals at exactly 2s gaps.
    record_uniform_arrivals(&registry, &flow_id, Duration::from_secs(2), 5);

    registry.classify_and_apply(&flow, &flow_id);
    assert_eq!(
        flow.priority(),
        10,
        "gap == background_gap_max should classify as background (10)"
    );
}

/// Test: gap exactly == interactive_gap_min (30s) → interactive.
#[test]
fn boundary_exact_interactive_gap_min() {
    let registry = make_registry();
    let flow = make_flow("boundary-inter", 50);
    let flow_id = FlowId::new("boundary-inter");

    // 5 arrivals at exactly 30s gaps.
    record_uniform_arrivals(&registry, &flow_id, Duration::from_secs(30), 5);

    registry.classify_and_apply(&flow, &flow_id);
    assert_eq!(
        flow.priority(),
        100,
        "gap == interactive_gap_min should classify as interactive (100)"
    );
}

// ---------------------------------------------------------------------------
// Hysteresis tests
// ---------------------------------------------------------------------------

/// Test: hysteresis blocks one-shot demotion from interactive.
///
/// Scenario:
/// 1. Flow starts at interactive priority (100).
/// 2. We record a burst of fast arrivals, but the median gap is NOT all fast.
///    The median includes some slow gaps → should stay interactive.
/// 3. Then we record a sustained fast burst where the last 3 gaps are ALL fast.
///    Now the flow should demote to background.
#[test]
fn hysteresis_blocks_one_shot_demotion() {
    let registry = make_registry();
    let flow = make_flow("hyst", 50);
    let flow_id = FlowId::new("hyst");

    // Step 1: promote to interactive by recording slow gaps.
    record_uniform_arrivals(&registry, &flow_id, Duration::from_secs(60), 5);
    registry.classify_and_apply(&flow, &flow_id);
    assert_eq!(flow.priority(), 100, "Should be interactive");

    // Step 2: record a mixed burst — mix of fast and slow gaps.
    // The last 3 gaps will NOT all be fast, so hysteresis should block.
    let t_base = Instant::now();
    // Arrivals at: 0, 1s, 2s, 60s, 120s
    // Gaps: 1s, 1s, 58s, 60s
    // Last 3 gaps: 1s, 58s, 60s — NOT all ≤ 2s.
    registry.record_arrival(&flow_id, t_base);
    registry.record_arrival(&flow_id, t_base + Duration::from_secs(1));
    registry.record_arrival(&flow_id, t_base + Duration::from_secs(2));
    registry.record_arrival(
        &flow_id,
        t_base + Duration::from_secs(2) + Duration::from_secs(58),
    );
    registry.record_arrival(
        &flow_id,
        t_base + Duration::from_secs(2) + Duration::from_secs(58) + Duration::from_secs(60),
    );

    registry.classify_and_apply(&flow, &flow_id);
    // Median gap is 29.5s or so — still near interactive range.
    // Even if median falls to background, hysteresis should block demotion
    // because last 3 gaps are not ALL fast.
    assert!(
        flow.priority() == 100 || flow.priority() == 50,
        "Should not demote to background when last 3 gaps are mixed (got {})",
        flow.priority()
    );

    // Step 3: now record enough fast arrivals so the last 3 gaps are ALL ≤ 2s.
    // Add many rapid arrivals so the rolling window shifts to fast gaps.
    // We need the last 3 gaps to be fast AND the median to be ≤ 2s.
    let t2 = Instant::now();
    // Record 15 more arrivals at 1s intervals (all fast).
    for i in 0..15u32 {
        registry.record_arrival(&flow_id, t2 + Duration::from_secs(i as u64));
    }

    registry.classify_and_apply(&flow, &flow_id);
    // Now the median gap is 1s (≤ 2s) AND the last 3 gaps are all 1s (≤ 2s).
    assert_eq!(
        flow.priority(),
        10,
        "Sustained fast burst should demote to background (10)"
    );
}

// ---------------------------------------------------------------------------
// Override blocking tests
// ---------------------------------------------------------------------------

/// Test: header override (source=1) blocks classification.
#[test]
fn override_header_blocks_classify() {
    let registry = make_registry();
    let flow = make_flow("override-header", 50);
    let flow_id = FlowId::new("override-header");

    // Set priority source to header (1).
    flow.set_priority_source(1);

    // Record rapid-fire arrivals that would normally demote to background.
    record_uniform_arrivals(&registry, &flow_id, Duration::from_millis(500), 5);

    registry.classify_and_apply(&flow, &flow_id);
    assert_eq!(
        flow.priority(),
        50,
        "Header override (source=1) should block classification"
    );
}

/// Test: admin override (source=2) blocks classification.
#[test]
fn override_admin_blocks_classify() {
    let registry = make_registry();
    let flow = make_flow("override-admin", 50);
    let flow_id = FlowId::new("override-admin");

    // Set priority source to admin (2).
    flow.set_priority_source(2);

    // Record rapid-fire arrivals.
    record_uniform_arrivals(&registry, &flow_id, Duration::from_millis(500), 5);

    registry.classify_and_apply(&flow, &flow_id);
    assert_eq!(
        flow.priority(),
        50,
        "Admin override (source=2) should block classification"
    );
}

/// Test: when policy is disabled, classify_and_apply does nothing.
#[test]
fn disabled_policy_skips_classification() {
    let policy = PriorityPolicy {
        enabled: false,
        interactive_gap_min: Duration::from_secs(30),
        background_gap_max: Duration::from_secs(2),
        sample_window: 20,
        min_samples: 3,
    };
    let classes = Priorities {
        interactive: 100,
        agent: 50,
        background: 10,
    };
    let registry = CadenceRegistry::new(Arc::new(policy), Arc::new(classes));
    let flow = make_flow("disabled", 50);
    let flow_id = FlowId::new("disabled");

    record_uniform_arrivals(&registry, &flow_id, Duration::from_millis(500), 5);

    registry.classify_and_apply(&flow, &flow_id);
    assert_eq!(
        flow.priority(),
        50,
        "Disabled policy should not change priority"
    );
}

// ---------------------------------------------------------------------------
// Median tie-breaking test
// ---------------------------------------------------------------------------

/// Test: median_gap picks lower-middle for even-length delta sequences.
///
/// With 6 arrivals at uniform gaps, there are 5 deltas (odd count → exact median).
/// With 5 arrivals, there are 4 deltas (even count → lower-middle index = 1).
#[test]
fn median_gap_even_count_lower_middle() {
    let policy = PriorityPolicy {
        enabled: true,
        interactive_gap_min: Duration::from_secs(30),
        background_gap_max: Duration::from_secs(2),
        sample_window: 20,
        min_samples: 3,
    };
    let classes = Priorities {
        interactive: 100,
        agent: 50,
        background: 10,
    };
    let registry = CadenceRegistry::new(Arc::new(policy), Arc::new(classes));
    let flow_id = FlowId::new("median-test");

    // Record 5 arrivals at varying gaps:
    // t0, t0+10s, t0+10s+1s, t0+10s+1s+100s, t0+10s+1s+100s+50s
    // Gaps: 10s, 1s, 100s, 50s
    // Sorted: [1s, 10s, 50s, 100s] — 4 deltas (even count).
    // Lower-middle (index 4/2 - 1 = 1) = 10s → agent (50).
    let t0 = Instant::now();
    registry.record_arrival(&flow_id, t0);
    registry.record_arrival(&flow_id, t0 + Duration::from_secs(10));
    registry.record_arrival(&flow_id, t0 + Duration::from_secs(11));
    registry.record_arrival(&flow_id, t0 + Duration::from_secs(111));
    registry.record_arrival(&flow_id, t0 + Duration::from_secs(161));

    // The median of [1s, 10s, 50s, 100s] with lower-middle picks 10s.
    // 10s is between 2s and 30s → agent.
    let flow = make_flow("median-test", 50);
    registry.classify_and_apply(&flow, &flow_id);
    assert_eq!(
        flow.priority(),
        50,
        "Lower-middle median (10s) should classify as agent (50)"
    );
}

// ---------------------------------------------------------------------------
// Regression tests for median index (odd-count off-by-one bug)
// ---------------------------------------------------------------------------

/// Regression test: odd delta count must pick the exact middle, not index 0.
///
/// 4 arrivals → 3 deltas (odd count). Sorted deltas [1s, 10s, 10s].
/// Correct median index = 1 (10s). Buggy code would pick index 0 (1s).
///
/// With background_gap_max=2s and interactive_gap_min=30s:
/// - Correct (10s): between 2s and 30s → agent (50)
/// - Buggy (1s): ≤ 2s → background (10)
#[test]
fn median_gap_odd_count_exact_middle() {
    let policy = PriorityPolicy {
        enabled: true,
        interactive_gap_min: Duration::from_secs(30),
        background_gap_max: Duration::from_secs(2),
        sample_window: 20,
        min_samples: 3,
    };
    let classes = Priorities {
        interactive: 100,
        agent: 50,
        background: 10,
    };
    let registry = CadenceRegistry::new(Arc::new(policy), Arc::new(classes));
    let flow_id = FlowId::new("odd-median");

    // 4 arrivals: t0, t0+1s, t0+11s, t0+21s
    // Deltas: [1s, 10s, 10s] → sorted [1s, 10s, 10s]
    // Odd count (3): exact middle index = 1 → 10s
    // Buggy code: 3/2 - 1 = 0 → index 0 → 1s
    let t0 = Instant::now();
    registry.record_arrival(&flow_id, t0);
    registry.record_arrival(&flow_id, t0 + Duration::from_secs(1));
    registry.record_arrival(&flow_id, t0 + Duration::from_secs(11));
    registry.record_arrival(&flow_id, t0 + Duration::from_secs(21));

    let flow = Flow::new(FlowId::new("odd-median"), 1.0, 50);
    registry.classify_and_apply(&flow, &flow_id);
    assert_eq!(
        flow.priority(),
        50,
        "Odd-count median (10s) should classify as agent (50), not background (10)"
    );
}

/// Regression test: single delta (2 arrivals) must not panic.
///
/// Uses min_samples=1 so classify actually computes median_gap with 1 delta.
#[test]
fn median_gap_single_delta_no_panic() {
    let policy = PriorityPolicy {
        enabled: true,
        interactive_gap_min: Duration::from_secs(30),
        background_gap_max: Duration::from_secs(2),
        sample_window: 20,
        min_samples: 1,
    };
    let classes = Priorities {
        interactive: 100,
        agent: 50,
        background: 10,
    };
    let registry = CadenceRegistry::new(Arc::new(policy), Arc::new(classes));
    let flow_id = FlowId::new("single-delta");

    // 2 arrivals → 1 delta (odd count). Index = 1/2 = 0.
    let t0 = Instant::now();
    registry.record_arrival(&flow_id, t0);
    registry.record_arrival(&flow_id, t0 + Duration::from_secs(5));

    let flow = Flow::new(FlowId::new("single-delta"), 1.0, 50);
    // This must not panic. Median = 5s → agent (50).
    registry.classify_and_apply(&flow, &flow_id);
    assert_eq!(
        flow.priority(),
        50,
        "Single delta (5s) should classify as agent (50)"
    );
}
