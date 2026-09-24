//! X11 平台壳主入口
//!
//! 实现 XIM 事件循环，集成 kime-core 引擎。
//!
//! 验收测试：
//! - cargo build -p platform-x11
//! - cargo test -p platform-x11
//!
//! 功能：
//! - XIM 协议：XOpenIM → XCreateIC → XFilterEvent
//! - XIMPreeditCallback 触发 Engine::key()
//! - X11 候选窗 + XIMGlyphPosition 实现光标跟随
//! - 解决微信等 XWayland 应用的光标跟随问题

use std::sync::Arc;

use kime_core::{config::Config, dict::Dict, Engine};
use platform_x11::xim::X11IM;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("[platform-x11] Starting X11 frontend...");

    let (conn, screen_num) = x11rb::connect(None)?;
    let conn = Arc::new(conn);

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    // 配置与 wayland 前端同源：Config::load() 读 ~/.config/kime/config.toml
    // （dict_path / shuangpin / punct_mode / fuzzy …）。此前这里写死
    // Config::default()，XWayland 应用（微信/QQ/Electron 走 XIM）拿不到用户的
    // 双拼方案与标点模式，行为与 wayland 前端分裂。
    let mut config = Config::load();
    if config.dict_path.is_empty() {
        config.dict_path = format!("{}/.local/share/kime/dict.sqlite3", home);
    }
    let dict = Dict::open(&config.dict_path)?;
    let engine = Engine::new(dict, config);

    let mut im = X11IM::new(conn, screen_num, engine)?;

    println!("[platform-x11] X11 frontend initialized successfully");
    println!("[platform-x11] XIM event loop starting...");

    im.run()?;

    Ok(())
}
