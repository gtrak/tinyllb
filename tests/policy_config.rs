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
    "TINYLLB__PRIORITY_POLICY__INTERACTIVE_GAP_MIN",
    "TINYLLB__PRIORITY_POLICY__BACKGROUND_GAP_MAX",
    "TINYLLB__PRIORITY_POLICY__SAMPLE_WINDOW",
    "TINYLLB__PRIORITY_POLICY__MIN_SAMPLES",
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
        pp.interactive_gap_min,
        std::time::Duration::from_secs(30),
        "interactive_gap_min should default to 30s"
    );
    assert_eq!(
        pp.background_gap_max,
        std::time::Duration::from_secs(2),
        "background_gap_max should default to 2s"
    );
    assert_eq!(pp.sample_window, 20, "sample_window should default to 20");
    assert_eq!(pp.min_samples, 3, "min_samples should default to 3");
}

#[test]
#[serial_test::serial]
fn validate_rejects_inverted_gaps() {
    let tmp = "/tmp/test_config_inverted_gaps.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
priority_policy:
  interactive_gap_min: 1s
  background_gap_max: 10s
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let result = with_env(&vars, config::load);
    std::fs::remove_file(tmp).ok();

    assert!(result.is_err(), "should error when interactive_gap_min <= background_gap_max");
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("interactive_gap_min"),
        "error should mention interactive_gap_min: {err}"
    );
}

#[test]
#[serial_test::serial]
fn validate_rejects_small_sample_window() {
    let tmp = "/tmp/test_config_small_window.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
priority_policy:
  sample_window: 2
  min_samples: 5
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let result = with_env(&vars, config::load);
    std::fs::remove_file(tmp).ok();

    assert!(
        result.is_err(),
        "should error when sample_window < min_samples"
    );
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("sample_window"),
        "error should mention sample_window: {err}"
    );
}
