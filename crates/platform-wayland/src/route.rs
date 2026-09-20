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
    /// 先上屏文本，再把按键本身转发给应用（Enter 上屏字母 + 真实回车）。
    /// 转发前同样销掉同键旧记账，release 配对按 Forward 规则走。
    CommitThenForward,
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
        Outcome::CommitAndForward(_) => PressRoute {
            action: PressAction::CommitThenForward,
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

/// 剪贴板模式一次 press 的裁决（M16）：纯函数，副作用（提交/重绘）归调用方。
///
/// - `C-;`（evdev 39）：切入（游标 0）/ 再按一次退出
/// - j(36)/k(37)：移动游标（有候选时）
/// - Enter(28)/空格(57)：提交当前游标候选
/// - Esc(1)/Backspace(14)：退出
/// - 其余键：退出模式并放行（`Forward`），调用方转交引擎
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipAction {
    /// 进入剪贴板模式（游标 0）
    Enter,
    /// 退出剪贴板模式（重绘走 popup_show）
    Exit,
    /// 游标移动到 `usize`
    Move(usize),
    /// 提交候选列表下标 `usize` 的文本并退出
    Commit(usize),
    /// 剪贴板模式内吞掉但无动作（空列表时的功能键）
    Swallow,
    /// 不在模式 / 未知键：放行给引擎
    Forward,
}

pub fn clip_route(
    active: bool,
    pick: Option<usize>,
    ctrl: bool,
    code: u32,
    n_candidates: usize,
) -> ClipAction {
    if ctrl && code == 39 {
        return if active {
            ClipAction::Exit
        } else {
            ClipAction::Enter
        };
    }
    let Some(pick) = pick else {
        return ClipAction::Forward;
    };
    match code {
        36 if n_candidates > 0 => ClipAction::Move((pick + 1).min(n_candidates - 1)), // j
        37 if n_candidates > 0 => ClipAction::Move(pick.saturating_sub(1)),           // k
        28 | 57 if n_candidates > 0 => {
            ClipAction::Commit(pick.min(n_candidates - 1)) // Enter/空格
        }
        // Esc/退格退出；空列表时导航/提交无处可去，也只退出
        1 | 14 | 36 | 37 | 28 | 57 => ClipAction::Exit,
        _ => ClipAction::Forward, // 其余：退出+放行
    }
}
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
        Outcome::CommitAndForward(text) => format!("CommitAndForward({text})"),
    };
    format!(
        "key code={code} ch={} mods={}{}{} -> {outcome_str}\n",
        ch.unwrap_or('-'),
        if ctrl { 'c' } else { '-' },
        if shift { 's' } else { '-' },
        if alt { 'a' } else { '-' },
    )
}
