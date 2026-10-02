//! 连接失败退避纯函数（`next_backoff_ms`）的测试：不碰环境、不连合成器，CI 直接跑。
//!
//! 「进程不再因连接失败退出」本身不写进程级用例——那要拉起真二进制并依赖渲染器
//! 字体初始化，环境脆；该契约由 src 侧 grep 锚点 + CI 编译 + 真机 smoke 兜底。
//! 这里只钉住退避规则本身：封顶丢失（无限翻倍 / 睡出 30s 以上）或复位丢失
//! （长会话断连仍按旧封顶值睡 30s）任一发生，下面的用例立刻红。

use std::time::Duration;

use platform_wayland::next_backoff_ms;

const BASE_MS: u32 = 1_000;
const MAX_MS: u32 = 30_000;

/// 短会话（连上就断）：1s 起、每次 ×2、30s 封顶且不越界。
///
/// 红的条件：翻倍公式丢失（首步不是 2_000）、封顶丢失（迭代中出现 >30_000 的值，
/// 或 16 步后仍没爬到 30_000）。
#[test]
fn short_session_doubles_and_caps() {
    let mut backoff = next_backoff_ms(BASE_MS, Duration::ZERO);
    assert_eq!(backoff, 2_000);
    // 定长迭代：封顶真丢了也能在这里红，而不是跑成死循环。
    for _ in 0..16 {
        backoff = next_backoff_ms(backoff, Duration::from_secs(1));
        assert!(backoff <= MAX_MS, "退避越过 30s 封顶: {backoff}ms");
    }
    assert_eq!(backoff, MAX_MS, "16 步内没爬到 30s 封顶: {backoff}ms");
    assert_eq!(
        next_backoff_ms(MAX_MS, Duration::from_secs(1)),
        MAX_MS,
        "封顶值继续翻倍应原地不动"
    );
}

/// 长会话（活满 60s 才算曾健康运行）：下次断连复位回 1s 基准。
///
/// 红的条件：复位丢失（30_000 睡完还带出更大的值）、复位值写错（不是 1_000）、
/// 健康门槛写错（59s 就复位，或 60s 不复位）。
#[test]
fn long_session_resets_to_base() {
    assert_eq!(
        next_backoff_ms(MAX_MS, Duration::from_secs(61)),
        BASE_MS,
        "活满 60s 的会话断连后应回到 1s 基准"
    );
    assert_eq!(
        next_backoff_ms(MAX_MS, Duration::from_secs(60)),
        BASE_MS,
        "60s 正好达门槛（>=）即复位"
    );
    assert_eq!(
        next_backoff_ms(MAX_MS, Duration::from_secs(59)),
        MAX_MS,
        "未达门槛不复位，封顶值原样带入下一次"
    );
    assert_eq!(
        next_backoff_ms(4_000, Duration::from_secs(61)),
        BASE_MS,
        "复位与当前值无关，一律回基准"
    );
}
