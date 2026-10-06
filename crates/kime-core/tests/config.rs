//! config 集成测试：原位于 src/config.rs 的内嵌测试模块（编译期测试配置），
//! 迁移后经 kime_core 公共 API（`kime_core::config::Config` /
//! `kime_core::Scheme`）驱动；`toml::from_str` 与 `tempfile::tempdir` 保持全限定。

use kime_core::config::Config;
use kime_core::Scheme;
use std::fs;
use std::io::Write;
use tempfile::NamedTempFile;

#[test]
fn test_valid_toml() {
    let mut config_file = NamedTempFile::new().unwrap();
    let toml_content = r#"
        dict_path = "test_dict.sqlite3"
        shuangpin = "xiaohe"
        page_size = 20
        fuzzy = ["zh=z"]
        "#;
    config_file.write_all(toml_content.as_bytes()).unwrap();
    let config = Config::load_from_path(config_file.path());
    assert_eq!(config.dict_path, "test_dict.sqlite3");
    assert!(matches!(config.shuangpin, Some(Scheme::Xiaohe)));
    assert_eq!(config.page_size, 20);
    assert_eq!(config.fuzzy, vec!["zh=z".to_string()]);
}

#[test]
fn test_invalid_toml() {
    let mut config_file = NamedTempFile::new().unwrap();
    let toml_content = "invalid toml content = = =";
    config_file.write_all(toml_content.as_bytes()).unwrap();
    let config = Config::load_from_path(config_file.path());
    assert_eq!(config.page_size, 10);
}

#[test]
fn test_default_roundtrip() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join(".config/kime/config.toml");
    let config = Config::load_from_path(&config_path);
    assert!(config_path.exists());
    let content = fs::read_to_string(&config_path).unwrap();
    let loaded: Config = toml::from_str(&content).unwrap();
    assert_eq!(config, loaded);
}

#[test]
fn test_ai_model_default() {
    let config = Config::default();
    assert_eq!(config.ai_model, "gpt-oss-120b");
}
