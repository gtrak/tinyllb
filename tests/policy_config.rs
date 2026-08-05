//! Tests for the `PriorityPolicy` configuration.

use tinyllb::config;

/// Helper: save and restore env vars for a test.
fn with_env<F, R>(vars: &[(&str, Option<&str>)], f: F) -> R
where
    F: FnOnce() -> R,
{
    let saved: Vec<(&str, Option<String>)> = vars
        .iter()
        .map(|(k, _)| {
            let v = std::env::var(*k).ok();
            (*k, v)
        })
        .collect();

    for (k, v) in vars {
        match v {
            Some(val) => std::env::set_var(k, val),
            None => std::env::remove_var(k),
        }
    }

    let result = f();

    for (k, v) in saved {
        match v {
            Some(ref val) => std::env::set_var(k, val),
            None => std::env::remove_var(k),
        }
    }

    result
}

/// Tests that should NOT have env overrides must explicitly clear them.
const ENV_OVERRIDES: &[&str] = &[
    "TINYLLB__SCHEDULER__MAX_ACTIVE_FLOWS",
    "TINYLLB__SCHEDULER__STARVATION_TIMEOUT",
    "TINYLLB__SCHEDULER__ALGORITHM",
    "TINYLLB__FLOWS__DEFAULT_WEIGHT",
    "TINYLLB__FLOWS__DEFAULT_PRIORITY",
    "TINYLLB__PRIORITIES__INTERACTIVE",
    "TINYLLB__PRIORITIES__AGENT",
    "TINYLLB__PRIORITIES__BACKGROUND",
    "TINYLLB__BACKPRESSURE__MODE",
    "TINYLLB__METRICS__ENDPOINT",
    "TINYLLB__SERVER__BIND",
    "TINYLLB__BACKEND__URL",
    "TINYLLB__PRIORITY_POLICY__ENABLED",
    "TINYLLB__PRIORITY_POLICY__IDLE_GAP_THRESHOLD",
    "TINYLLB__PRIORITY_POLICY__AGENTIC_SUSPECTED_THRESHOLD",
    "TINYLLB__PRIORITY_POLICY__AGENTIC_CONFIRMED_THRESHOLD",
];

/// Build a vars slice that sets the given vars AND clears all env overrides.
fn no_env_overrides<'a>(extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, Option<&'a str>)> {
    let mut vars: Vec<(&'a str, Option<&'a str>)> =
        extra.iter().map(|(k, v)| (*k, Some(*v))).collect();
    for k in ENV_OVERRIDES {
        vars.push((*k, None));
    }
    vars
}

#[test]
#[serial_test::serial]
fn priority_policy_default_is_enabled() {
    let tmp = "/tmp/test_config_minimal_priority.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let cfg = with_env(&vars, || {
        config::load().expect("should load minimal config")
    });
    std::fs::remove_file(tmp).ok();

    let pp = &cfg.priority_policy;
    assert!(pp.enabled, "enabled should default to true");
    assert_eq!(
        pp.idle_gap_threshold,
        std::time::Duration::from_secs(30),
        "idle_gap_threshold should default to 30s"
    );
    assert_eq!(pp.agentic_suspected_threshold, 5, "agentic_suspected_threshold should default to 5");
    assert_eq!(pp.agentic_confirmed_threshold, 12, "agentic_confirmed_threshold should default to 12");
}

#[test]
#[serial_test::serial]
fn validate_rejects_inverted_thresholds() {
    let tmp = "/tmp/test_config_inverted_thresholds.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
priority_policy:
  agentic_confirmed_threshold: 3
  agentic_suspected_threshold: 5
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let result = with_env(&vars, config::load);
    std::fs::remove_file(tmp).ok();

    assert!(
        result.is_err(),
        "should error when agentic_confirmed_threshold <= agentic_suspected_threshold"
    );
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("agentic_confirmed_threshold"),
        "error should mention agentic_confirmed_threshold: {err}"
    );
}

#[test]
#[serial_test::serial]
fn validate_rejects_zero_suspected_threshold() {
    let tmp = "/tmp/test_config_zero_suspected.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
priority_policy:
  agentic_suspected_threshold: 0
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let result = with_env(&vars, config::load);
    std::fs::remove_file(tmp).ok();

    assert!(
        result.is_err(),
        "should error when agentic_suspected_threshold == 0"
    );
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("agentic_suspected_threshold"),
        "error should mention agentic_suspected_threshold: {err}"
    );
}
