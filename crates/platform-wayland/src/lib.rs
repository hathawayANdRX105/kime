//! platform-wayland: input-method-v2 壳，候选词直接画进 zwp_input_popup_surface_v2。

pub mod clipboard_watch;
pub mod context_batch;
pub mod keyboard;
pub mod mode_badge;
pub mod render;
pub mod repeat;
pub mod route;
pub mod tray;

pub use render::{Layout, PlacedItem, Renderer};

/// 连接失败的退避间隔：给定**当前**退避值与刚结束那次连接的存活时长，算出下一次
/// 断连该睡多久。`kime-ime` 由 `kime-switch` 的 2 秒循环拉起，进程在合成器重启
/// 窗口期退出只会退化成反复起停空转（还可能触发 mango 键盘销毁的会话级崩溃，
/// 见 #101），所以连接类失败一律走「reset → 退避 → 重试」，进程只有
/// `should_exit → Done` 一条正常退出路；本函数就是那条退避路的全部规则。
///
/// - `session_lived >= 60s`：这次连接曾健康运行 → 下次断连复位回 1s 基准，
///   之后再按 ×2 递增（合成器重启恢复后不必跟着旧封顶值干等 30s）；
/// - `session_lived < 60s`：连接仍不稳 → `current_ms × 2` 并封顶 30s，从 1s 起。
///
/// `current_ms` 由调用方从 1_000 起维护（本函数只翻倍不设下限，传 0 会睡出 0ms）。
///
/// # 示例
///
/// ```
/// # use platform_wayland::next_backoff_ms;
/// # use std::time::Duration;
/// // 短会话：1s 起翻倍
/// assert_eq!(next_backoff_ms(1_000, Duration::ZERO), 2_000);
/// // 长会话：复位回基准
/// assert_eq!(next_backoff_ms(30_000, Duration::from_secs(61)), 1_000);
/// ```
pub fn next_backoff_ms(current_ms: u32, session_lived: std::time::Duration) -> u32 {
    const BASE_BACKOFF_MS: u32 = 1_000;
    const MAX_BACKOFF_MS: u32 = 30_000;
    const HEALTHY_SESSION: std::time::Duration = std::time::Duration::from_secs(60);
    if session_lived >= HEALTHY_SESSION {
        BASE_BACKOFF_MS
    } else {
        current_ms.saturating_mul(2).min(MAX_BACKOFF_MS)
    }
}

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
