//! 配置（TOML，~/.config/kime/config.toml）。字段即契约；
//! M4 的模糊音/标点风格等实现时再加，不预铺字段。

use kime_shuangpin::Scheme;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Config {
    /// SQLite 库路径（词库 + 用户词同库）
    pub dict_path: String,
    /// None = 全拼
    pub shuangpin: Option<Scheme>,
    /// OpenAI 兼容端点；None = 关闭 AI 预测
    pub ai_endpoint: Option<String>,
    /// 模糊音替换对（"zh=z"、"n=l"、"an=ang"）；空 = 关闭
    pub fuzzy: Vec<String>,
    /// Binary dict path (optional); loads ~/.local/share/kime/dict.bin if exists
    pub dict_bin_path: Option<String>,
    /// Page size for UI (default 10)
    pub page_size: usize,
}

impl Default for Config {
    fn default() -> Self {
        // ponytail: HOME unset on Windows/CI runners — fall back so the engine
        // is always constructible in tests.
        let dict_path = std::env::var("HOME")
            .ok()
            .map(|h| format!("{}/.local/share/kime/dict.sqlite3", h))
            .unwrap_or_else(|| ".kime-dict.sqlite3".to_string());
        Self {
            dict_path,
            shuangpin: None,
            ai_endpoint: None,
            fuzzy: Vec::new(),
            dict_bin_path: None,
            page_size: 10,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        if let Ok(p) = std::env::var("KIME_CONFIG_PATH") {
            return Self::load_from_path(Path::new(&p));
        }
        let home = match std::env::var("HOME") {
            Ok(h) => h,
            Err(_) => {
                eprintln!("[kime] 错误：无法获取 HOME 环境变量");
                return Config::default();
            }
        };
        let config_dir = Path::new(&home).join(".config").join("kime");
        let config_path = config_dir.join("config.toml");
        Self::load_from_path(&config_path)
    }

    pub fn load_from_path(config_path: &Path) -> Self {
        if config_path.exists() {
            if let Ok(content) = fs::read_to_string(config_path) {
                match toml::from_str::<Config>(&content) {
                    Ok(config) => return config,
                    Err(e) => {
                        eprintln!("[kime] 警告：config.toml 解析失败: {}，回退到默认配置", e);
                        return Config::default();
                    }
                }
            }
        }

        if let Some(parent) = config_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let default_config = Config::default();
        if let Ok(toml_str) = toml::to_string(&default_config) {
            let _ = fs::write(config_path, toml_str);
        }
        default_config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        // Should fall back to default
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
}
