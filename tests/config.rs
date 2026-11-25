mod helpers;
use debugbundle::{Config, ConfigError, Level};
use std::{sync::Arc, time::Duration};
fn config(service: &str) -> Config {
    Config {
        service: service.into(),
        ..helpers::configured(Arc::new(helpers::Recorder::default()))
    }
}

#[test]
fn configuration_rejects_invalid_limits_and_preserves_zero_sampling() {
    let mut config = config("checkout");
    assert!(config.validate().is_ok());
    config.sample_rate = 0.0;
    assert!(config.validate().is_ok());
    config.sample_rate = f64::NAN;
    assert_eq!(config.validate(), Err(ConfigError::InvalidLimit));
    config.sample_rate = 1.0;
    config.request_timeout = Duration::ZERO;
    assert!(config.validate().is_err());
    config.request_timeout = Duration::from_secs(5);
    config.max_buffered_events = 1001;
    assert!(config.validate().is_err());
}
#[test]
fn missing_credentials_are_not_healthy_connected_configuration() {
    let config = Config::default();
    assert_eq!(config.validate(), Err(ConfigError::MissingToken));
    assert_eq!(config.log_level, Level::Warning);
    assert!(Config {
        enabled: false,
        ..config
    }
    .validate()
    .is_ok());
}

#[test]
fn reader_pin_accepts_unique_features_in_any_order_and_integer_json_numbers() {
    let config = Config {
        evidence_capabilities: Some(
            serde_json::json!({"version":1.0,"features":["recovery_v1","exception_evidence_v1"]}),
        ),
        ..config("service")
    };
    assert!(config.validate().is_ok());
    assert_eq!(
        debugbundle::Client::new(config).status().state,
        debugbundle::ConnectionStatus::Healthy
    );
}

#[test]
fn discovery_identity_and_endpoint_values_cannot_carry_credentials() {
    for identity in [
        "dbundle_proj_SYNTHETIC_SECRET",
        "password=SYNTHETIC_SECRET",
        "service\nAuthorization: secret",
    ] {
        assert!(config(identity).validate().is_err());
        assert!(Config {
            environment: identity.into(),
            ..config("service")
        }
        .validate()
        .is_err());
    }
    for endpoint in [
        "https://api.example.test/v1/events?token=SYNTHETIC_SECRET",
        "https://api.example.test/dbundle_proj_SYNTHETIC_SECRET",
        "https://api.example.test/v1/events?%74oken=SYNTHETIC_SECRET",
    ] {
        assert!(Config {
            endpoint: endpoint.into(),
            ..config("service")
        }
        .validate()
        .is_err());
    }
    assert!(Config {
        endpoint: "https://api.example.test/v1/events?mode=a%20b&channel=stable".into(),
        ..config("service")
    }
    .validate()
    .is_ok());
}

#[test]
fn redact_exact_fields_builder_validation_and_endpoint_matching() {
    let half_broad: Vec<String> = (0..64).map(|i| format!("broad_{i}")).collect();
    let half_exact: Vec<String> = (0..64).map(|i| format!("exact_{i}")).collect();
    let ok = config("service")
        .with_redact_fields(half_broad.clone())
        .with_redact_exact_fields(half_exact.clone());
    assert!(ok.validate().is_ok());
    assert_eq!(ok.redact_fields.len(), 64);
    assert_eq!(ok.redact_exact_fields.len(), 64);

    let mut over_exact = half_exact;
    over_exact.push("exact_64".into());
    let over = config("service")
        .with_redact_fields(half_broad)
        .with_redact_exact_fields(over_exact);
    assert_eq!(over.validate(), Err(ConfigError::InvalidPrivacy));
    assert_eq!(
        debugbundle::Client::new(over).status().reason,
        Some("invalid_configuration")
    );

    assert_eq!(
        config("service")
            .with_redact_exact_fields(["   "])
            .validate(),
        Err(ConfigError::InvalidPrivacy)
    );
    assert_eq!(
        config("service")
            .with_redact_exact_fields(["k".repeat(65)])
            .validate(),
        Err(ConfigError::InvalidPrivacy)
    );

    assert_eq!(
        Config {
            endpoint: "https://api.example.test/v1/events?tag=SYNTHETIC_SECRET".into(),
            ..config("service").with_redact_exact_fields(["tag"])
        }
        .validate(),
        Err(ConfigError::InvalidEndpoint)
    );
    assert!(Config {
        endpoint: "https://api.example.test/v1/events?tag_name=script".into(),
        ..config("service").with_redact_exact_fields(["tag"])
    }
    .validate()
    .is_ok());
}

#[cfg(not(feature = "http"))]
#[test]
fn core_only_requires_an_explicit_custom_transport() {
    assert_eq!(
        Config::new("synthetic", "service").validate(),
        Err(ConfigError::MissingTransport)
    );
    assert!(config("service").validate().is_ok());
}
