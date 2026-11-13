use kime_core::config::Config;
use kime_core::config::PunctMode;

#[test]
fn status_returns_current_mode() {
    let config = Config::default();
    // Default should be Chinese mode
    assert_eq!(config.punct_mode, PunctMode::Chinese);
}

#[test]
fn config_punct_mode_roundtrip() {
    let config = Config {
        punct_mode: PunctMode::English,
        ..Config::default()
    };
    assert_eq!(config.punct_mode, PunctMode::English);
    let config = Config {
        punct_mode: PunctMode::Chinese,
        ..Config::default()
    };
    assert_eq!(config.punct_mode, PunctMode::Chinese);
}
