//! Tests for X-LLM-Priority header parsing and override application.
//!
//! Verifies:
//! - Explicit header pins a flow to the chosen priority class.
//! - `auto` unsets a prior pin and resumes the heuristic.
//! - Unknown header values are ignored (no state change).
//! - Case-insensitive parsing of priority tokens.
//! - Flow ID and priority header are parsed independently.
//! - Admin-registered flows get priority_source = 2.

use axum::http::{HeaderMap, HeaderValue};
use bytes::Bytes;
use llm_qdisc_proxy::config::Priorities;
use llm_qdisc_proxy::flow::{
    FlowId, FlowRegistry, FlowRegistration, PriorityClass, identify,
};

fn make_classes() -> Priorities {
    Priorities {
        interactive: 100,
        agent: 50,
        background: 10,
    }
}

/// Test: explicit header pins flow to chosen priority class.
#[test]
fn explicit_header_pins_flow() {
    let registry = FlowRegistry::new(1.0, 50);
    let classes = make_classes();
    let flow_id = FlowId::new("test-flow");

    registry.apply_priority_override(
        &flow_id,
        Some(PriorityClass::Interactive),
        false,
        &classes,
    );

    let flow = registry.get_or_create(flow_id.clone());
    assert_eq!(flow.priority(), 100);
    assert_eq!(flow.priority_source(), 1);
}

/// Test: `auto` header unsets a prior pin and resumes heuristic.
#[test]
fn auto_header_unsets_prior_pin() {
    let registry = FlowRegistry::new(1.0, 50);
    let classes = make_classes();
    let flow_id = FlowId::new("test-flow");

    // First pin to interactive (source=1, priority=100).
    registry.apply_priority_override(
        &flow_id,
        Some(PriorityClass::Interactive),
        false,
        &classes,
    );

    let flow = registry.get_or_create(flow_id.clone());
    assert_eq!(flow.priority(), 100);
    assert_eq!(flow.priority_source(), 1);

    // Then unset with auto.
    registry.apply_priority_override(&flow_id, None, true, &classes);

    let flow = registry.get_or_create(flow_id.clone());
    assert_eq!(flow.priority_source(), 0);
    assert_eq!(flow.priority(), 50); // agent default
}

/// Test: unknown header value is ignored (no state change).
#[test]
fn unknown_header_value_ignored() {
    let mut headers = HeaderMap::new();
    headers.insert("x-llm-priority", HeaderValue::from_static("garbage"));
    let body = Bytes::from_static(b"{}");

    let resolved = identify::resolve(&headers, &body);
    assert!(resolved.priority_override.is_none());
    assert!(!resolved.unset_override);
}

/// Test: case-insensitive parsing of priority tokens.
#[test]
fn case_insensitive_parsing() {
    // Uppercase
    let mut headers = HeaderMap::new();
    headers.insert("x-llm-priority", HeaderValue::from_static("INTERACTIVE"));
    let body = Bytes::from_static(b"{}");
    let resolved = identify::resolve(&headers, &body);
    assert_eq!(resolved.priority_override, Some(PriorityClass::Interactive));

    // Mixed case
    let mut headers = HeaderMap::new();
    headers.insert("x-llm-priority", HeaderValue::from_static("Background"));
    let body = Bytes::from_static(b"{}");
    let resolved = identify::resolve(&headers, &body);
    assert_eq!(resolved.priority_override, Some(PriorityClass::Background));
}

/// Test: X-LLM-Flow-ID and X-LLM-Priority are parsed independently.
#[test]
fn x_llm_flow_id_takes_precedence() {
    let mut headers = HeaderMap::new();
    headers.insert("x-llm-flow-id", HeaderValue::from_static("foo"));
    headers.insert("x-llm-priority", HeaderValue::from_static("background"));
    let body = Bytes::from_static(b"{}");

    let resolved = identify::resolve(&headers, &body);
    assert_eq!(resolved.flow_id.to_string(), "foo");
    assert_eq!(resolved.priority_override, Some(PriorityClass::Background));
}

/// Test: absent header produces no override.
#[test]
fn absent_header_no_override() {
    let headers = HeaderMap::new();
    let body = Bytes::from_static(b"{}");

    let resolved = identify::resolve(&headers, &body);
    assert!(resolved.priority_override.is_none());
    assert!(!resolved.unset_override);
}

/// Test: admin-registered flows get priority_source = 2.
#[test]
fn admin_register_sets_source_2() {
    let registry = FlowRegistry::new(1.0, 50);
    let flow_id = FlowId::new("admin-flow");

    registry.register(FlowRegistration {
        id: flow_id.clone(),
        weight: 1.0,
        priority: 100,
    });

    let flow = registry.get_or_create(flow_id.clone());
    assert_eq!(flow.priority_source(), 2);
    assert_eq!(flow.priority(), 100);
}

/// Test: updating an existing flow via register sets priority_source = 2.
#[test]
fn admin_update_sets_source_2() {
    let registry = FlowRegistry::new(1.0, 50);
    let flow_id = FlowId::new("admin-flow");

    // Create via get_or_create (source = 0).
    registry.get_or_create(flow_id.clone());
    let flow = registry.get_or_create(flow_id.clone());
    assert_eq!(flow.priority_source(), 0);

    // Register updates the flow and sets source to 2.
    registry.register(FlowRegistration {
        id: flow_id.clone(),
        weight: 2.0,
        priority: 80,
    });

    let flow = registry.get_or_create(flow_id.clone());
    assert_eq!(flow.priority_source(), 2);
    assert_eq!(flow.priority(), 80);
    assert_eq!(flow.weight(), 2.0);
}

/// Test: no header keeps existing state.
#[test]
fn no_header_keeps_existing_state() {
    let registry = FlowRegistry::new(1.0, 50);
    let classes = make_classes();
    let flow_id = FlowId::new("test-flow");

    // Pin to interactive.
    registry.apply_priority_override(
        &flow_id,
        Some(PriorityClass::Interactive),
        false,
        &classes,
    );

    // Call with no override and no unset — state should be preserved.
    registry.apply_priority_override(&flow_id, None, false, &classes);

    let flow = registry.get_or_create(flow_id.clone());
    assert_eq!(flow.priority(), 100);
    assert_eq!(flow.priority_source(), 1);
}

/// Test: `auto` is idempotent when no prior pin exists.
#[test]
fn auto_is_idempotent_when_no_pin() {
    let registry = FlowRegistry::new(1.0, 50);
    let classes = make_classes();
    let flow_id = FlowId::new("test-flow");

    // No prior pin; calling unset is safe.
    registry.apply_priority_override(&flow_id, None, true, &classes);

    let flow = registry.get_or_create(flow_id.clone());
    assert_eq!(flow.priority_source(), 0);
    assert_eq!(flow.priority(), 50); // agent default
}
