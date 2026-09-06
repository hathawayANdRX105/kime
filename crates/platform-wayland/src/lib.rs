//! platform-wayland crate for Wayland input method support
//!
//! Provides layer-shell candidate window rendering with cosmic-text
//! and integration with kime input method engine.

pub mod render;
pub mod tray;
pub mod window;

pub use render::Candidate;
pub use render::Renderer;
