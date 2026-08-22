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
fn load_example_yaml() {
    with_env(
        &no_env_overrides(&[("CONFIG_PATH", "config.example.yaml")]),
        || {
            let cfg = config::load().expect("should load config.example.yaml");

            assert!(
                cfg.backend.url.as_str() == "http://localhost:8000/"
                    || cfg.backend.url.as_str() == "http://localhost:8000",
                "backend url: {}",
                cfg.backend.url
            );
            assert_eq!(
                cfg.scheduler.max_active_flows, 4,
                "max_active_flows from yaml"
            );
            assert_eq!(cfg.flows.default_weight, 1.0, "default_weight from yaml");
            assert_eq!(
                cfg.priorities.interactive, 100,
                "interactive priority from yaml"
            );
            assert_eq!(cfg.priorities.agent, 50, "agent priority from yaml");
            assert_eq!(
                cfg.priorities.background, 10,
                "background priority from yaml"
            );
        },
    );
}

#[test]
#[serial_test::serial]
fn env_override_max_active_flows() {
    // Only clear CONFIG_PATH; keep the env override.
    with_env(
        &[
            ("CONFIG_PATH", Some("config.example.yaml")),
            ("TINYLLB__SCHEDULER__MAX_ACTIVE_FLOWS", Some("8")),
        ],
        || {
            let cfg = config::load().expect("should load with env override");
            assert_eq!(cfg.scheduler.max_active_flows, 8, "env override applied");
        },
    );
}

#[test]
#[serial_test::serial]
fn invalid_max_active_flows_returns_error() {
    let tmp = "/tmp/test_config_invalid_flows.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
scheduler:
  max_active_flows: 0
  starvation_timeout: 300s
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let result = with_env(&vars, config::load);
    std::fs::remove_file(tmp).ok();

    assert!(result.is_err(), "should error on max_active_flows == 0");
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("max_active_flows"),
        "error mentions max_active_flows: {err}"
    );
}

#[test]
#[serial_test::serial]
fn invalid_default_weight_returns_error() {
    let tmp = "/tmp/test_config_invalid_weight.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
scheduler:
  max_active_flows: 4
  starvation_timeout: 300s
flows:
  default_weight: 0
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let result = with_env(&vars, config::load);
    std::fs::remove_file(tmp).ok();

    assert!(result.is_err(), "should error on default_weight == 0");
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("default_weight"),
        "error mentions default_weight: {err}"
    );
}

#[test]
#[serial_test::serial]
fn defaults_applied_when_field_omitted() {
    let tmp = "/tmp/test_config_minimal.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:9999
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let cfg = with_env(&vars, || {
        config::load().expect("should load minimal config")
    });
    std::fs::remove_file(tmp).ok();

    // backend.url comes from yaml
    assert!(
        cfg.backend.url.as_str() == "http://localhost:9999/"
            || cfg.backend.url.as_str() == "http://localhost:9999",
        "backend url: {}",
        cfg.backend.url
    );
    // everything else is defaults
    assert_eq!(cfg.scheduler.max_active_flows, 4);
    assert_eq!(
        cfg.scheduler.starvation_timeout,
        std::time::Duration::from_secs(300)
    );
    assert_eq!(cfg.flows.default_weight, 1.0);
    assert_eq!(cfg.flows.default_priority, 50);
    assert_eq!(cfg.priorities.interactive, 100);
    assert_eq!(cfg.priorities.agent, 50);
    assert_eq!(cfg.priorities.background, 10);
    assert_eq!(cfg.metrics.endpoint, "/metrics");
    assert_eq!(
        cfg.server.bind,
        "0.0.0.0:8080".parse::<std::net::SocketAddr>().unwrap()
    );
}

#[test]
#[serial_test::serial]
fn invalid_metrics_interval_zero_returns_error() {
    let tmp = "/tmp/test_config_zero_metrics_interval.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
  metrics_interval: 0s
scheduler:
  max_active_flows: 4
  starvation_timeout: 300s
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let result = with_env(&vars, config::load);
    std::fs::remove_file(tmp).ok();

    assert!(result.is_err(), "should error on metrics_interval == 0s");
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("metrics_interval"),
        "error mentions metrics_interval: {err}"
    );
}

#[test]
#[serial_test::serial]
fn top_level_kv_policy_errors_with_migration_message() {
    let tmp = "/tmp/test_config_legacy_kv_policy.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
backpressure:
  mode: blocking
kv_policy:
  enabled: true
  reject_threshold: 0.95
  delay_threshold: 0.80
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let result = with_env(&vars, config::load);
    std::fs::remove_file(tmp).ok();

    assert!(result.is_err(), "top-level kv_policy should error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("backpressure") && err.contains("kv_policy"),
        "error should explain the migration: {err}"
    );
}

#[test]
#[serial_test::serial]
fn kv_pressure_defaults_to_disabled_empty_thresholds() {
    let tmp = "/tmp/test_config_kv_pressure_defaults.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:9999
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let cfg = with_env(&vars, || {
        config::load().expect("should load minimal config")
    });
    std::fs::remove_file(tmp).ok();

    assert!(!cfg.scheduler.kv_pressure.enabled);
    assert!(cfg.scheduler.kv_pressure.thresholds.is_empty());
}

#[test]
#[serial_test::serial]
fn kv_pressure_full_ladder_parses() {
    let tmp = "/tmp/test_config_kv_pressure_ladder.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
scheduler:
  max_active_flows: 4
  kv_pressure:
    enabled: true
    thresholds:
      - at: 0.5
        max_flows: 3
      - at: 0.8
        max_flows: 2
      - at: 0.95
        max_flows: 1
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let cfg = with_env(&vars, || {
        config::load().expect("should load kv_pressure ladder")
    });
    std::fs::remove_file(tmp).ok();

    assert!(cfg.scheduler.kv_pressure.enabled);
    assert_eq!(
        cfg.scheduler.kv_pressure.thresholds,
        vec![
            config::KvPressureThreshold {
                at: 0.5,
                max_flows: 3,
            },
            config::KvPressureThreshold {
                at: 0.8,
                max_flows: 2,
            },
            config::KvPressureThreshold {
                at: 0.95,
                max_flows: 1,
            },
        ]
    );
}

#[test]
#[serial_test::serial]
fn kv_pressure_enabled_empty_thresholds_errors() {
    let tmp = "/tmp/test_config_kv_pressure_empty.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
scheduler:
  max_active_flows: 4
  kv_pressure:
    enabled: true
    thresholds: []
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let result = with_env(&vars, config::load);
    std::fs::remove_file(tmp).ok();

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("kv_pressure.thresholds must not be empty"),
        "error should mention empty thresholds: {err}"
    );
}

#[test]
#[serial_test::serial]
fn kv_pressure_unsorted_thresholds_error() {
    let tmp = "/tmp/test_config_kv_pressure_unsorted.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
scheduler:
  max_active_flows: 4
  kv_pressure:
    enabled: true
    thresholds:
      - at: 0.8
        max_flows: 2
      - at: 0.5
        max_flows: 3
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let result = with_env(&vars, config::load);
    std::fs::remove_file(tmp).ok();

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("strictly ascending"),
        "error should mention ordering: {err}"
    );
}

#[test]
#[serial_test::serial]
fn kv_pressure_at_out_of_range_errors() {
    let tmp = "/tmp/test_config_kv_pressure_at_range.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
scheduler:
  max_active_flows: 4
  kv_pressure:
    enabled: true
    thresholds:
      - at: 1.5
        max_flows: 2
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let result = with_env(&vars, config::load);
    std::fs::remove_file(tmp).ok();

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("at must be in [0, 1]"),
        "error should mention at range: {err}"
    );
}

#[test]
#[serial_test::serial]
fn kv_pressure_max_flows_zero_errors() {
    let tmp = "/tmp/test_config_kv_pressure_zero_flows.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
scheduler:
  max_active_flows: 4
  kv_pressure:
    enabled: true
    thresholds:
      - at: 0.5
        max_flows: 0
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let result = with_env(&vars, config::load);
    std::fs::remove_file(tmp).ok();

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("max_flows must be in [1, max_active_flows]"),
        "error should mention max_flows range: {err}"
    );
}

#[test]
#[serial_test::serial]
fn kv_pressure_max_flows_exceeds_max_active_flows_errors() {
    let tmp = "/tmp/test_config_kv_pressure_too_many_flows.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
scheduler:
  max_active_flows: 4
  kv_pressure:
    enabled: true
    thresholds:
      - at: 0.5
        max_flows: 8
"#,
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let result = with_env(&vars, config::load);
    std::fs::remove_file(tmp).ok();

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("max_flows must be in [1, max_active_flows]"),
        "error should mention max_flows range: {err}"
    );
}

#[test]
#[serial_test::serial]
fn llamacpp_slots_defaults_to_none() {
    let tmp = "/tmp/test_config_llamacpp_slots_none.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:9999
"#
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let cfg = with_env(&vars, || {
        config::load().expect("should load minimal config")
    });
    std::fs::remove_file(tmp).ok();

    assert_eq!(cfg.backend.llamacpp_slots, None);
}

#[test]
#[serial_test::serial]
fn llamacpp_slots_parses_some() {
    let tmp = "/tmp/test_config_llamacpp_slots_some.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
  llamacpp_slots: 4
"#
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let cfg = with_env(&vars, || {
        config::load().expect("should parse llamacpp_slots: 4")
    });
    std::fs::remove_file(tmp).ok();

    assert_eq!(cfg.backend.llamacpp_slots, Some(4));
}

#[test]
#[serial_test::serial]
fn llamacpp_slots_zero_rejected() {
    let tmp = "/tmp/test_config_llamacpp_slots_zero.yaml";
    std::fs::write(
        tmp,
        r#"
backend:
  url: http://localhost:8000
  llamacpp_slots: 0
"#
    )
    .expect("write temp yaml");

    let vars = no_env_overrides(&[("CONFIG_PATH", tmp)]);
    let result = with_env(&vars, config::load);
    std::fs::remove_file(tmp).ok();

    assert!(result.is_err(), "should error on llamacpp_slots == 0");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("llamacpp_slots"),
        "error should mention llamacpp_slots: {err}"
    );
}
