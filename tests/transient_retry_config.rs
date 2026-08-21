//! Tests for the `TransientRetry` configuration (`backend.transient_retry`).

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
    "TINYLLB__BACKEND__TRANSIENT_RETRY__MAX_ATTEMPTS",
    "TINYLLB__BACKEND__TRANSIENT_RETRY__BACKOFF_START",
    "TINYLLB__BACKEND__TRANSIENT_RETRY__BACKOFF_MAX",
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
fn defaults_parse() {
    let tmp = "/tmp/test_config_transient_retry_defaults.yaml";
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

    let tr = &cfg.backend.transient_retry;
    assert_eq!(tr.max_attempts, 3, "max_attempts should default to 3");
    assert_eq!(
        tr.backoff_start,
        std::time::Duration::from_millis(500),
        "backoff_start should default to 500ms"
    );
    assert_eq!(
        tr.backoff_max,
        std::time::Duration::from_secs(4),
        "backoff_max should default to 4s"
    );
}

#[test]
#[serial_test::serial]
fn invalid_backoff_rejected() {
    let tmp = "/tmp/test_config_transient_retry_invalid.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
  transient_retry:
    max_attempts: 3
    backoff_start: 1s
    backoff_max: 500ms
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let result = with_env(&vars, config::load);
    std::fs::remove_file(tmp).ok();

    assert!(
        result.is_err(),
        "should error when backoff_max < backoff_start"
    );
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("backoff_max must be >= backoff_start"),
        "error should mention backoff_max >= backoff_start: {err}"
    );
}

#[test]
#[serial_test::serial]
fn env_override_works() {
    let tmp = "/tmp/test_config_transient_retry_env.yaml";
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
        ("TINYLLB__BACKEND__TRANSIENT_RETRY__MAX_ATTEMPTS", Some("7")),
        ("TINYLLB__BACKEND__TRANSIENT_RETRY__BACKOFF_START", Some("2s")),
        ("TINYLLB__BACKEND__TRANSIENT_RETRY__BACKOFF_MAX", Some("10s")),
        // Clear all other env overrides except the three we want.
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
        ("TINYLLB__RETRY_POLICY__ENABLED", None),
        ("TINYLLB__RETRY_POLICY__MAX_RETRIES", None),
        ("TINYLLB__RETRY_POLICY__TEMPERATURE_STEP", None),
        ("TINYLLB__RETRY_POLICY__MAX_TEMPERATURE", None),
        ("TINYLLB__RETRY_POLICY__DEFAULT_TEMPERATURE", None),
    ];
    let cfg = with_env(&vars, || {
        config::load().expect("should load with env overrides")
    });
    std::fs::remove_file(tmp).ok();

    let tr = &cfg.backend.transient_retry;
    assert_eq!(
        tr.max_attempts, 7,
        "max_attempts should be 7 from env"
    );
    assert_eq!(
        tr.backoff_start,
        std::time::Duration::from_secs(2),
        "backoff_start should be 2s from env"
    );
    assert_eq!(
        tr.backoff_max,
        std::time::Duration::from_secs(10),
        "backoff_max should be 10s from env"
    );
}
