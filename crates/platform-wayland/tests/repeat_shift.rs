//! 壳内纯状态机测试：长按自动重复（KeyRepeat）+ Shift 手势（ShiftComposer）。
//! 不起真实 wayland 连接；时钟全部是注入的毫秒数。

use platform_wayland::repeat::{
    is_repeatable_edit, KeyRepeat, ShiftComposer, ShiftGesture, ShiftRelease, REPEAT_DELAY_MS,
    REPEAT_INTERVAL_MS,
};

const BACKSPACE: u32 = 14;
const CTRL_F: u32 = 41; // KEY_F
const CTRL_H: u32 = 43; // KEY_H
const CTRL_B: u32 = 48; // KEY_B
const LETTER_A: u32 = 30; // 不在重复集内

// ---------- repeat 状态机：press→重复合成→release 停止 ----------

#[test]
fn only_composition_edit_keys_are_repeatable() {
    assert!(is_repeatable_edit(BACKSPACE));
    assert!(is_repeatable_edit(CTRL_F));
    assert!(is_repeatable_edit(CTRL_H));
    assert!(is_repeatable_edit(CTRL_B));
    // 字母/功能键不许起表（长按 a 连打是引擎的事，不是这里的）。
    assert!(!is_repeatable_edit(LETTER_A));
    assert!(!is_repeatable_edit(1)); // Esc
}

#[test]
fn press_ticks_after_delay_then_at_interval_until_release() {
    let mut r = KeyRepeat::default();
    r.arm(BACKSPACE, 1000);
    assert_eq!(r.next_due(), Some(1000 + REPEAT_DELAY_MS));

    // 延迟未到：不合成。
    assert_eq!(r.tick(1000 + REPEAT_DELAY_MS - 1), None);
    // 到点：第一次合成，下次间隔 33ms。
    assert_eq!(r.tick(1000 + REPEAT_DELAY_MS), Some(BACKSPACE));
    assert_eq!(
        r.next_due(),
        Some(1000 + REPEAT_DELAY_MS + REPEAT_INTERVAL_MS)
    );
    assert_eq!(
        r.tick(1000 + REPEAT_DELAY_MS + REPEAT_INTERVAL_MS),
        Some(BACKSPACE)
    );
    // 未到点不抢跑。
    assert_eq!(
        r.tick(1000 + REPEAT_DELAY_MS + 2 * REPEAT_INTERVAL_MS - 1),
        None
    );

    // 物理 release：停表，之后永不合成。
    assert!(r.release(BACKSPACE));
    assert_eq!(r.next_due(), None);
    assert_eq!(r.tick(999_999), None);
}

#[test]
fn release_of_other_key_keeps_repeat_alive() {
    let mut r = KeyRepeat::default();
    r.arm(CTRL_F, 0);
    // 按住 F 再按 B 之类：别的键 release 不动这张表。
    assert!(!r.release(CTRL_B));
    assert_eq!(r.tick(REPEAT_DELAY_MS), Some(CTRL_F));
}

#[test]
fn re_press_rearms_with_fresh_delay() {
    let mut r = KeyRepeat::default();
    r.arm(BACKSPACE, 0);
    r.release(BACKSPACE);
    r.arm(BACKSPACE, 5000);
    assert_eq!(r.next_due(), Some(5000 + REPEAT_DELAY_MS));
    assert_eq!(r.tick(5000 + REPEAT_DELAY_MS), Some(BACKSPACE));
}

#[test]
fn clear_drops_pending_repeat() {
    let mut r = KeyRepeat::default();
    r.arm(BACKSPACE, 0);
    r.clear();
    assert_eq!(r.next_due(), None);
    assert_eq!(r.tick(REPEAT_DELAY_MS), None);
}

// ---------- Shift 手势：tap 切换 vs hold 临时英文 ----------

fn armed(c: &ShiftComposer) -> bool {
    matches!(c.gesture, ShiftGesture::Armed { .. })
}

#[test]
fn shift_tap_resolves_to_toggle() {
    let mut c = ShiftComposer::default();
    assert!(c.on_shift_press(100));
    assert!(armed(&c));
    assert_eq!(c.on_shift_release(), ShiftRelease::Toggle);
    assert_eq!(c.gesture, ShiftGesture::Idle);
}

#[test]
fn letter_during_hold_passthroughs_and_release_does_not_toggle() {
    let mut c = ShiftComposer::default();
    c.on_shift_press(100);
    // 按住期间打字母：接管 → 透传，引擎不接触 → 模式无从翻转（验收 3 的状态机面）。
    assert!(c.on_key_press());
    assert_eq!(c.gesture, ShiftGesture::HoldActive { keys_typed: 1 });
    assert!(c.on_key_press()); // 再打一个
    assert_eq!(c.gesture, ShiftGesture::HoldActive { keys_typed: 2 });
    assert_eq!(c.on_shift_release(), ShiftRelease::NoToggle);
    assert_eq!(c.gesture, ShiftGesture::Idle);
}

#[test]
fn function_key_during_hold_counts_too() {
    let mut c = ShiftComposer::default();
    c.on_shift_press(0);
    assert!(c.on_key_press()); // 方向键/Home 等走同一条透传
    assert_eq!(c.on_shift_release(), ShiftRelease::NoToggle);
}

#[test]
fn both_shifts_only_last_release_resolves() {
    let mut c = ShiftComposer::default();
    assert!(c.on_shift_press(0)); // 左 Shift
    assert!(c.on_shift_press(10)); // 右 Shift（不重置 press_time，不叠 Armed）
    assert_eq!(c.on_shift_release(), ShiftRelease::None); // 左松开：右还按着
    assert_eq!(c.on_shift_release(), ShiftRelease::Toggle); // 最后松开才裁决
}

#[test]
fn release_without_any_press_is_noop() {
    let mut c = ShiftComposer::default();
    assert_eq!(c.on_shift_release(), ShiftRelease::None);
    assert!(!c.on_key_press()); // 空闲期按键 = 走老路径，不接管
    assert!(!c.active());
}

#[test]
fn key_press_while_idle_is_not_gesture_taken() {
    let mut c = ShiftComposer::default();
    assert!(!c.on_key_press());
}

#[test]
fn tap_after_tap_rearms() {
    let mut c = ShiftComposer::default();
    c.on_shift_press(0);
    assert_eq!(c.on_shift_release(), ShiftRelease::Toggle);
    // 下一轮点击同样成立（手势复位干净）。
    c.on_shift_press(1000);
    assert!(armed(&c));
    assert_eq!(c.on_shift_release(), ShiftRelease::Toggle);
}

#[test]
fn reset_drops_mid_armed_gesture() {
    let mut c = ShiftComposer::default();
    c.on_shift_press(0);
    c.reset();
    assert!(!c.active());
    // reset 后的 release（计数已归零、手势 Idle）不产生切换裁决。
    assert_eq!(c.on_shift_release(), ShiftRelease::None);
}
