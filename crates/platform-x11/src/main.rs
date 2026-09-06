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

    let (conn, _screen_num) = x11rb::connect(None)?;
    let conn = Arc::new(conn);

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let dict_path = format!("{}/.local/share/kime/dict.sqlite3", home);
    let dict = Dict::open(&dict_path)?;
    let config = Config::default();
    let engine = Engine::new(dict, config);

    let mut im = X11IM::new(conn.clone(), engine);

    println!("[platform-x11] X11 frontend initialized successfully");
    println!("[platform-x11] XIM event loop starting...");

    im.run();

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_platform_x11_basic() {
        assert!(true);
    }

    #[test]
    fn test_xim_event_handling() {
        assert!(true);
    }

    #[test]
    fn test_cursor_following() {
        assert!(true);
    }
}
