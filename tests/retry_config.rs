//! Tests for the `RetryPolicy` configuration.

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
    "TINYLLB__RETRY_POLICY__ENABLED",
    "TINYLLB__RETRY_POLICY__MAX_RETRIES",
    "TINYLLB__RETRY_POLICY__TEMPERATURE_STEP",
    "TINYLLB__RETRY_POLICY__MAX_TEMPERATURE",
    "TINYLLB__RETRY_POLICY__DEFAULT_TEMPERATURE",
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
fn retry_policy_defaults_disabled() {
    let tmp = "/tmp/test_config_retry_defaults.yaml";
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

    let rp = &cfg.retry_policy;
    assert!(!rp.enabled, "enabled should default to false");
    assert_eq!(rp.max_retries, 2, "max_retries should default to 2");
    assert_eq!(
        rp.temperature_step, 0.3,
        "temperature_step should default to 0.3"
    );
    assert_eq!(
        rp.max_temperature, 1.5,
        "max_temperature should default to 1.5"
    );
    assert_eq!(
        rp.default_temperature, 0.0,
        "default_temperature should default to 0.0"
    );
}

#[test]
#[serial_test::serial]
fn env_override_enables_and_sets_max_retries() {
    let tmp = "/tmp/test_config_retry_env.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
"#,
    )
    .expect("write temp yaml");

    let vars = vec![
        ("CONFIG_PATH", Some(tmp)),
        ("TINYLLB__RETRY_POLICY__ENABLED", Some("true")),
        ("TINYLLB__RETRY_POLICY__MAX_RETRIES", Some("5")),
        // Clear all other env overrides except the two we want.
        ("TINYLLB__SCHEDULER__MAX_ACTIVE_FLOWS", None),
        ("TINYLLB__SCHEDULER__STARVATION_TIMEOUT", None),
        ("TINYLLB__SCHEDULER__ALGORITHM", None),
        ("TINYLLB__FLOWS__DEFAULT_WEIGHT", None),
        ("TINYLLB__FLOWS__DEFAULT_PRIORITY", None),
        ("TINYLLB__PRIORITIES__INTERACTIVE", None),
        ("TINYLLB__PRIORITIES__AGENT", None),
        ("TINYLLB__PRIORITIES__BACKGROUND", None),
        ("TINYLLB__BACKPRESSURE__MODE", None),
        ("TINYLLB__METRICS__ENDPOINT", None),
        ("TINYLLB__SERVER__BIND", None),
        ("TINYLLB__BACKEND__URL", None),
        ("TINYLLB__PRIORITY_POLICY__ENABLED", None),
        ("TINYLLB__PRIORITY_POLICY__INTERACTIVE_GAP_MIN", None),
        ("TINYLLB__PRIORITY_POLICY__BACKGROUND_GAP_MAX", None),
        ("TINYLLB__PRIORITY_POLICY__SAMPLE_WINDOW", None),
        ("TINYLLB__PRIORITY_POLICY__MIN_SAMPLES", None),
        ("TINYLLB__RETRY_POLICY__TEMPERATURE_STEP", None),
        ("TINYLLB__RETRY_POLICY__MAX_TEMPERATURE", None),
        ("TINYLLB__RETRY_POLICY__DEFAULT_TEMPERATURE", None),
    ];
    let cfg = with_env(&vars, || {
        config::load().expect("should load with env overrides")
    });
    std::fs::remove_file(tmp).ok();

    assert!(cfg.retry_policy.enabled, "enabled should be true from env");
    assert_eq!(
        cfg.retry_policy.max_retries, 5,
        "max_retries should be 5 from env"
    );
}

#[test]
#[serial_test::serial]
fn validate_rejects_zero_max_retries() {
    let tmp = "/tmp/test_config_retry_zero_max.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
retry_policy:
  enabled: true
  max_retries: 0
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let result = with_env(&vars, config::load);
    std::fs::remove_file(tmp).ok();

    assert!(result.is_err(), "should error on max_retries == 0");
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("retry_policy.max_retries"),
        "error should mention retry_policy.max_retries: {err}"
    );
}

#[test]
#[serial_test::serial]
fn validate_rejects_zero_temperature_step() {
    let tmp = "/tmp/test_config_retry_zero_step.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
retry_policy:
  enabled: true
  temperature_step: 0.0
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let result = with_env(&vars, config::load);
    std::fs::remove_file(tmp).ok();

    assert!(result.is_err(), "should error on temperature_step == 0.0");
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("retry_policy.temperature_step"),
        "error should mention retry_policy.temperature_step: {err}"
    );
}

#[test]
#[serial_test::serial]
fn validate_rejects_max_below_default() {
    let tmp = "/tmp/test_config_retry_max_below_default.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
retry_policy:
  enabled: true
  default_temperature: 1.0
  max_temperature: 0.5
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let result = with_env(&vars, config::load);
    std::fs::remove_file(tmp).ok();

    assert!(
        result.is_err(),
        "should error when max_temperature < default_temperature"
    );
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("retry_policy.max_temperature must be >= default_temperature"),
        "error should mention max_temperature >= default_temperature: {err}"
    );
}

#[test]
#[serial_test::serial]
fn validate_rejects_max_above_2() {
    let tmp = "/tmp/test_config_retry_max_above_2.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
retry_policy:
  enabled: true
  max_temperature: 2.5
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let result = with_env(&vars, config::load);
    std::fs::remove_file(tmp).ok();

    assert!(
        result.is_err(),
        "should error when max_temperature > 2.0"
    );
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("<= 2.0"),
        "error should mention <= 2.0: {err}"
    );
}

#[test]
#[serial_test::serial]
fn validation_skipped_when_disabled() {
    let tmp = "/tmp/test_config_retry_disabled_validation.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
retry_policy:
  enabled: false
  max_retries: 0
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let cfg = with_env(&vars, || {
        config::load().expect("should load when validation is skipped")
    });
    std::fs::remove_file(tmp).ok();

    assert!(!cfg.retry_policy.enabled, "enabled should be false");
    assert_eq!(
        cfg.retry_policy.max_retries, 0,
        "max_retries should be 0 (validation skipped)"
    );
}
