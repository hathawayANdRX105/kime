//! platform-wayland: input-method-v2 壳，候选词直接画进 zwp_input_popup_surface_v2。

pub mod context_batch;
pub mod keyboard;
pub mod render;
pub mod repeat;
pub mod route;
pub mod tray;

pub use render::{Layout, PlacedItem, Renderer};

/// press/release 配对记账。不变式：`keys` 含某键 ⇔ 该键**最近一次 press 被引擎消费**且
/// release 尚未到达。踩过的坑：长按自动重复会让同一键中途从「消费」变「放行」（组合被删空），
/// 若转发 press 时不销记，release 被残留标记吃掉 → 应用以为键按住不放 → 幽灵连发退格。
#[derive(Default)]
pub struct SwallowTracker {
    keys: std::collections::HashSet<u32>,
}

impl SwallowTracker {
    /// press 被引擎消费：release 也须吞掉。
    pub fn consume(&mut self, key: u32) {
        self.keys.insert(key);
    }

    /// press 转发给应用：销掉同键旧记账，保证后续 release 同样转发。
    pub fn forward(&mut self, key: u32) {
        self.keys.remove(&key);
    }

    /// release 到达。true = 吞掉（对应 press 被消费过）；false = 转发给应用。
    pub fn release(&mut self, key: u32) -> bool {
        self.keys.remove(&key)
    }

    /// grab 重建/失效：在途键状态作废。
    pub fn clear(&mut self) {
        self.keys.clear();
    }
}
