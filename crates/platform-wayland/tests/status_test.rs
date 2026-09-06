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
    let mut config = Config::default();
    config.punct_mode = PunctMode::English;
    assert_eq!(config.punct_mode, PunctMode::English);
    config.punct_mode = PunctMode::Chinese;
    assert_eq!(config.punct_mode, PunctMode::Chinese);
}
