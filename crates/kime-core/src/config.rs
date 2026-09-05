//! 配置（TOML，~/.config/kime/config.toml）。字段即契约；
//! M4 的模糊音/标点风格等实现时再加，不预铺字段。

use kime_shuangpin::Scheme;

#[derive(Debug)]
pub struct Config {
    /// SQLite 库路径（词库 + 用户词同库）
    pub dict_path: String,
    /// None = 全拼
    pub shuangpin: Option<Scheme>,
    /// OpenAI 兼容端点；None = 关闭 AI 预测
    pub ai_endpoint: Option<String>,
    /// 模糊音替换对（"zh=z"、"n=l"、"an=ang"）；空 = 关闭
    pub fuzzy: Vec<String>,
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
        }
    }
}
