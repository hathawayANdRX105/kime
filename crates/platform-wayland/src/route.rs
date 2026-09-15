//! 壳层按键路由的纯状态机：Key 构造唯一真源 + Outcome→动作裁决 + release 配对。
//!
//! main.rs 的真按键事件只做「收集旗标 → 调这里 → 执行返回动作」，让
//! key_char → Key → engine.key → Outcome → 消费/放行 的整条决策链可以被
//! tests/key_routing.rs 原样钉住——引擎单测绿而壳层没人测，正是第五轮
//! 「C-f 真机不生效」能藏住的结构性原因（同族前车之鉴：b1834e6 键码错表）。

use kime_core::{Key, Outcome};

use crate::repeat::{is_repeatable_edit, KeyRepeat};
use crate::SwallowTracker;

/// 壳层构造引擎 [`Key`] 的唯一路径：code 已是 evdev 空间（wayland 原生值），
/// shift 旗标只对 Shift 键码本身为真——引擎靠它区分 Shift 点击与普通字符，
/// 按住 Shift 打出的大写字符旗标仍是 false（与 rime 的 modifier mask 一致）。
pub fn shell_key(code: u32, ch: Option<char>, ctrl: bool, alt: bool) -> Key {
    Key {
        ch,
        code,
        shift: matches!(code, 42 | 54),
        ctrl,
        alt,
    }
}

/// Shift 按住期间（手势窗口内）一次 press 的透传裁决：
/// 只有**字母**原样透传（大写英文形态）；标点走引擎（rime 的 ascii_composer
/// 只影响字母，punctuator 不受 shift 位影响——Shift hold 打标点出中文标点）。
/// `alt` 组合永远直通。此前标点也透传，是「中文括号逗号偶尔变英文」的头号根因。
pub fn shift_holds_passthrough(in_gesture_window: bool, ch: Option<char>, alt: bool) -> bool {
    let letter = ch.is_some_and(|c| c.is_ascii_alphabetic());
    (in_gesture_window && letter) || alt
}

/// 一次真 press 的路由裁决要做的动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PressAction {
    /// 放行给应用：销掉同键旧记账后转发 press。
    Forward,
    /// 引擎消费/上屏：吞掉 release，并刷 preedit 或 commit。
    Consume,
    /// 合成 tick 撞上组合已空：给应用补一次完整的 press+release 点击。
    Tap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PressRoute {
    pub action: PressAction,
    /// 起自动重复表：仅「真 press + Consumed + 组合仍在」的可重复编辑键。
    pub arm: bool,
    /// 撤该键的重复表：真 press 一旦不被消费/上屏，旧表必须撤，
    /// 否则合成 tick 会继续喂一个应用已接管的键。
    pub disarm: bool,
}

/// main.rs 的 engine_press 的全部副作用决策，收敛为这一处纯函数。
/// `composing` = 引擎处理完这键后组合是否还在；`synthetic` = 长按合成 tick。
pub fn route_press(code: u32, synthetic: bool, composing: bool, outcome: &Outcome) -> PressRoute {
    match outcome {
        Outcome::Consumed => PressRoute {
            action: PressAction::Consume,
            arm: !synthetic && composing && is_repeatable_edit(code),
            disarm: false,
        },
        Outcome::Commit(_) => PressRoute {
            action: PressAction::Consume,
            arm: false,
            disarm: true,
        },
        Outcome::Ignored if synthetic => PressRoute {
            action: PressAction::Tap,
            arm: false,
            disarm: false,
        },
        Outcome::Ignored => PressRoute {
            action: PressAction::Forward,
            arm: false,
            disarm: true,
        },
    }
}

/// 真键 release 的路由：先撤该键重复表，再按 press 记账决定吞放。
/// 返回 true = release 须转发给应用。Shift 的 release 在调用方先过手势裁决。
pub fn route_release(code: u32, swallowed: &mut SwallowTracker, repeat: &mut KeyRepeat) -> bool {
    // 只撤自己：重复表里的 code 与 release 的 code 不同（如 Ctrl/F1 的伴随键）时不动。
    repeat.release(code);
    !swallowed.release(code)
}

/// 逐键路由日志的行格式（真机定罪用的统一契约）：
/// `key code=<u32> ch=<char|-> mods=<c?><s?><a?> -> <Outcome>`
/// 纯格式化，落盘在 main.rs；钉在 tests/key_routing.rs 防格式漂移。
pub fn key_log_line(
    code: u32,
    ch: Option<char>,
    ctrl: bool,
    alt: bool,
    shift: bool,
    outcome: &Outcome,
) -> String {
    let outcome_str = match outcome {
        Outcome::Consumed => "Consumed".to_string(),
        Outcome::Ignored => "Ignored".to_string(),
        Outcome::Commit(text) => format!("Commit({text})"),
    };
    format!(
        "key code={code} ch={} mods={}{}{} -> {outcome_str}\n",
        ch.unwrap_or('-'),
        if ctrl { 'c' } else { '-' },
        if shift { 's' } else { '-' },
        if alt { 'a' } else { '-' },
    )
}
