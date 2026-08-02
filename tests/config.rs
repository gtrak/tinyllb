use llm_qdisc_proxy::config;

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
    "LLM_QDISC__SCHEDULER__MAX_ACTIVE_FLOWS",
    "LLM_QDISC__SCHEDULER__STARVATION_TIMEOUT",
    "LLM_QDISC__SCHEDULER__ALGORITHM",
    "LLM_QDISC__FLOWS__DEFAULT_WEIGHT",
    "LLM_QDISC__FLOWS__DEFAULT_PRIORITY",
    "LLM_QDISC__PRIORITIES__INTERACTIVE",
    "LLM_QDISC__PRIORITIES__AGENT",
    "LLM_QDISC__PRIORITIES__BACKGROUND",
    "LLM_QDISC__BACKPRESSURE__MODE",
    "LLM_QDISC__METRICS__ENDPOINT",
    "LLM_QDISC__SERVER__BIND",
    "LLM_QDISC__BACKEND__URL",
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
                cfg.scheduler.algorithm,
                config::Algorithm::Drr,
                "algorithm from yaml"
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
            ("LLM_QDISC__SCHEDULER__MAX_ACTIVE_FLOWS", Some("8")),
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
    assert_eq!(cfg.scheduler.algorithm, config::Algorithm::Drr);
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
