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
}

impl Default for Config {
    fn default() -> Self {
        todo!("M1: dict_path 默认 ~/.local/share/kime/dict.sqlite3")
    }
}
