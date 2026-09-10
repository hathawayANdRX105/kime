//! platform-wayland: input-method 壳 + egui 候选窗。

pub mod panel;
pub mod render;
pub mod tray;
pub mod window;

pub use render::Candidate;
pub use render::Renderer;
