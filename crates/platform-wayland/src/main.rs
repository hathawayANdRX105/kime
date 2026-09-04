//! M3: input-method-v2 平台壳。
//!
//! 流程：input_method_manager_v2 activate → grab_keyboard →
//! 按键 → `kime_core::Engine::key` →
//! preedit_string / 候选窗（layer-shell + cosmic-text）→ commit_string 上屏。
//! spike 顺序：先「按啥 commit 啥」在 mango 跑通，再接 Engine。
//!
//! XWayland 应用不支持（input-method-v2 只管原生 wayland），已知并接受。

fn main() {
    todo!("M3: wayland-client + wayland-protocols-misc::input_method_v2")
}
