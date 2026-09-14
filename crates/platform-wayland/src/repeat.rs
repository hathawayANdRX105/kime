//! 壳内纯状态机：组合内长按自动重复 + Shift 手势（rime ascii_composer 语义）。
//!
//! 为什么不依赖合成器投递 autorepeat（查证结论，2026-09）：
//! - libinput `src/evdev-fallback.c::fallback_process_key` 开头即
//!   `/* ignore kernel key repeat */ if (e->value == 2) return;`，内核重复被吞；
//! - wlroots `types/wlr_keyboard.c` 不自造重复事件（只存 repeat_info 并转发后端事件）；
//! - mango `src/input/keyboard.c::keyboard_repeat` 的自研 wl_event_source 重复定时器
//!   只回调 `keyboard_check_keybinding`（全局键位），从不再走
//!   `mango_im_keyboard_grab_forward_key` —— IM grab 每按住一次只收到一个 press
//!   和一个 release。协议里 `repeat_info` 事件的存在本身即「IM 客户端自理重复」的设计意图。
//! 所以组合内连删/连移必须由壳自己合成。
//!
//! 时钟统一用调用方传入的毫秒数（main.rs 用 CLOCK_MONOTONIC，与合成器 key 事件 time
//! 同起点）；本模块不碰时间也不碰 wayland 连接，纯函数供 tests/ 覆盖。

/// 长按到第一次合成重复的延迟（ms）。不做配置——与 X11/rime 默认观感一致即可。
pub const REPEAT_DELAY_MS: u64 = 500;
/// 后续重复间隔（ms），≈30 次/秒。
pub const REPEAT_INTERVAL_MS: u64 = 33;

const KEY_BACKSPACE: u32 = 14;
const KEY_F: u32 = 41;
const KEY_H: u32 = 43;
const KEY_B: u32 = 48;

/// 组合内可自动重复的编辑键：Backspace、C-f、C-h、C-b（evdev 码）。
/// 启动与否还要调用方门控「outcome==Consumed 且组合非空」，见 main.rs。
pub fn is_repeatable_edit(code: u32) -> bool {
    matches!(code, KEY_BACKSPACE | KEY_F | KEY_H | KEY_B)
}

/// 单槽重复定时器：按住期间同一物理键的每次真 press 重新 arm；合成 tick 只顺延。
/// ponytail: 单槽——先后按住两个可重复键时只跟最后一个，真需要再说。
#[derive(Default)]
pub struct KeyRepeat {
    active: Option<(u32, u64)>, // (keycode, next_due_ms)
}

impl KeyRepeat {
    /// 真 press 被消费且组合非空 → 起表。
    pub fn arm(&mut self, code: u32, now: u64) {
        self.active = Some((code, now + REPEAT_DELAY_MS));
    }

    /// 定时到期检查。到点返回该键并把下次到期推到 now+INTERVAL；没到点返回 None。
    pub fn tick(&mut self, now: u64) -> Option<u32> {
        let (code, due) = self.active?;
        if now < due {
            return None;
        }
        self.active = Some((code, now + REPEAT_INTERVAL_MS));
        Some(code)
    }

    /// 事件循环 poll 的超时材料：下次合成时刻。
    pub fn next_due(&self) -> Option<u64> {
        self.active.map(|(_, due)| due)
    }

    /// 物理 release。true = 正好是重复中的键，已停表。
    pub fn release(&mut self, code: u32) -> bool {
        if self.active.is_some_and(|(c, _)| c == code) {
            self.active = None;
            true
        } else {
            false
        }
    }

    /// grab 失效/重建等一切状态作废。
    pub fn clear(&mut self) {
        self.active = None;
    }
}

/// Shift 手势阶段。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShiftGesture {
    #[default]
    Idle,
    /// Shift 已按下、还没打过别的键：松开最后一次 Shift 即「点击切换」。
    Armed { press_time: u64 },
    /// Shift 按住期间打过别的键：临时英文已发生，松开不许切模式。
    HoldActive { keys_typed: u32 },
}

/// 最后一次 Shift 松开时的裁决。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShiftRelease {
    /// 还有另一个 Shift 按着，或手势根本没进过 Armed（组合内走老路径）——什么也别做。
    None,
    /// 点击（Armed 且无其他键）→ 把这次 Shift 补交给引擎切中英。
    Toggle,
    /// 按住打过键 → 模式不变，手势复位。
    NoToggle,
}

/// rime `ascii_composer` 语义裁决器：
/// 点击 Shift=切换中英；按住 Shift 期间打字=临时英文透传且不改模式。
/// 判定全在这里，引擎只在 Toggle 裁决时收到那一次 Shift「press」。
#[derive(Default)]
pub struct ShiftComposer {
    pub gesture: ShiftGesture,
    /// 左右 Shift 叠按计数：只有最后松开的那次才裁决。
    shifts_down: u8,
}

impl ShiftComposer {
    /// Shift press。返回 true = 手势接管了这一下（press 既不喂引擎也不透传，
    /// 裁决推迟到 release）；调用方仅在「引擎就绪且无组合」时喂进来。
    pub fn on_shift_press(&mut self, now: u64) -> bool {
        self.shifts_down += 1;
        if self.gesture == ShiftGesture::Idle {
            self.gesture = ShiftGesture::Armed { press_time: now };
        }
        true
    }

    /// 手势活跃（Armed/HoldActive）期间的非 Shift press：
    /// true = 该键走透传（临时英文/功能键直通），并计入 keys_typed。
    pub fn on_key_press(&mut self) -> bool {
        match self.gesture {
            ShiftGesture::Idle => false,
            ShiftGesture::Armed { .. } => {
                self.gesture = ShiftGesture::HoldActive { keys_typed: 1 };
                true
            }
            ShiftGesture::HoldActive { keys_typed } => {
                self.gesture = ShiftGesture::HoldActive {
                    keys_typed: keys_typed + 1,
                };
                true
            }
        }
    }

    /// 任意 Shift release。见 [`ShiftRelease`]。
    pub fn on_shift_release(&mut self) -> ShiftRelease {
        self.shifts_down = self.shifts_down.saturating_sub(1);
        if self.shifts_down > 0 {
            return ShiftRelease::None;
        }
        let verdict = match self.gesture {
            ShiftGesture::Armed { .. } => ShiftRelease::Toggle,
            ShiftGesture::HoldActive { .. } => ShiftRelease::NoToggle,
            ShiftGesture::Idle => ShiftRelease::None,
        };
        self.gesture = ShiftGesture::Idle;
        verdict
    }

    /// 手势是否接管着当前按键（release 分支不查它，接管期一切键都不许漏给引擎）。
    pub fn active(&self) -> bool {
        self.gesture != ShiftGesture::Idle
    }

    /// grab 失效/重建：状态作废。
    pub fn reset(&mut self) {
        self.gesture = ShiftGesture::Idle;
        self.shifts_down = 0;
    }
}
