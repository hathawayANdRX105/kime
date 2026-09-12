//! platform-wayland: input-method-v2 壳，候选词直接画进 zwp_input_popup_surface_v2。

pub mod render;
pub mod tray;

pub use render::{Layout, PlacedItem, Renderer};
