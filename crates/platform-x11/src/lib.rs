//! X11 平台前端实现
//!
//! 实现 XIM（X Input Method）协议集成，解决微信等 XWayland 应用的光标跟随问题。
//!
//! 功能：
//! - XIM 协议：XOpenIM → XCreateIC → XFilterEvent
//! - XIMPreeditCallback 触发 Engine::key()
//! - X11 候选窗绘制及 XIMGlyphPosition 光标追踪
//! - 共享 kime-core 引擎，不修改引擎代码
//!
//! # 架构
//!
//! 1. xim.rs - XIM 协议实现（基于 x11rb）
//! 2. window.rs - 候选窗管理
//! 3. render.rs - X11 绘图渲染

pub mod render;
pub mod window;
pub mod xim;

pub use render::Renderer;
pub use window::CandidateWindow;
pub use xim::X11IM;
