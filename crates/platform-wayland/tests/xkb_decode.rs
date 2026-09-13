//! xkb 解码正确性的离线证明（工单第 2 条）。
//!
//! 真机路径是合成器 keymap 事件的 fd → 文本 → `Keyboard::set_keymap`；这里把
//! 同一份 XKB V1 文本直接喂进同一个函数，断言 shift 层与老手写码表漏掉的键位。
//! 键图来源：系统 xkb 数据编译 us/pc105 后 `get_as_string` 回文本（与真机送来的
//! 东西同格式）。唯一只能真机验的是「合成器确实把这份协议的 modifiers 掩码按
//! wl 约定发来」——bit0=shift 的换名逻辑在 keyboard.rs 里，本文件用同一入口测。

use platform_wayland::keyboard::Keyboard;
use xkbcommon::xkb;

fn us_keyboard() -> Keyboard {
    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = xkb::Keymap::new_from_names(
        &ctx,
        "evdev",
        "pc105",
        "us",
        "",
        None,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .expect("系统 xkb 数据不可用：编译 us 键图失败");
    let text = keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
    let mut kb = Keyboard::new();
    assert!(
        kb.set_keymap(&text),
        "xkb 自己产出的文本必须能被 set_keymap 装载"
    );
    kb
}

// evdev keycode（linux/input-event-codes.h）
const KEY_ESC: u32 = 1;
const KEY_1: u32 = 2;
const KEY_9: u32 = 10;
const KEY_A: u32 = 30;
const KEY_APOSTROPHE: u32 = 40;
const KEY_SEMICOLON: u32 = 39;
const KEY_TAB: u32 = 15;
const KEY_ENTER: u32 = 28;
const KEY_LEFTSHIFT: u32 = 42;
const SPACE: u32 = 57;
const KEY_COMMA: u32 = 51;
const KEY_M: u32 = 50;
const KEY_PERIOD: u32 = 52;
const KEY_SLASH: u32 = 53;

/// wl_keyboard.modifiers 的 bit0 = shift。
const WL_SHIFT: u32 = 1 << 0;

#[test]
fn no_keymap_yet_decodes_to_none() {
    let kb = Keyboard::new();
    assert_eq!(kb.key_char(KEY_A), None, "键图未就绪时不得凭空造字符");
}

#[test]
fn base_layer_covers_punctuation_old_table_missed() {
    let kb = us_keyboard();
    assert_eq!(kb.key_char(KEY_COMMA), Some(','));
    assert_eq!(kb.key_char(KEY_PERIOD), Some('.'));
    assert_eq!(kb.key_char(KEY_SLASH), Some('/'));
    assert_eq!(kb.key_char(KEY_SEMICOLON), Some(';'));
    assert_eq!(kb.key_char(KEY_APOSTROPHE), Some('\''));
    assert_eq!(kb.key_char(KEY_M), Some('m'));
}

#[test]
fn shift_layer_now_reachable() {
    // 老码表完全没有 shift 层：这些以前永远是 None → 引擎见不到 → 应用吐 ASCII。
    let mut kb = us_keyboard();
    kb.update_mods(WL_SHIFT, 0, 0, 0);
    assert_eq!(kb.key_char(KEY_1), Some('!'));
    assert_eq!(kb.key_char(KEY_9), Some('('));
    assert_eq!(kb.key_char(KEY_SLASH), Some('?'));
    assert_eq!(kb.key_char(KEY_APOSTROPHE), Some('"'));
    assert_eq!(kb.key_char(KEY_SEMICOLON), Some(':'));
    assert_eq!(kb.key_char(KEY_M), Some('M'));
}

#[test]
fn mods_release_restores_base_layer() {
    let mut kb = us_keyboard();
    kb.update_mods(WL_SHIFT, 0, 0, 0);
    kb.update_mods(0, 0, 0, 0);
    assert_eq!(kb.key_char(KEY_SLASH), Some('/'));
    assert_eq!(kb.key_char(KEY_1), Some('1'));
}

#[test]
fn functional_keys_and_space_stay_charless() {
    // 引擎靠 `ch.is_none() && code == …` 匹配空格提交/Enter/退格/Esc/Shift 切换，
    // 这些必须留在 code 路径上。
    let kb = us_keyboard();
    for code in [KEY_ESC, KEY_ENTER, KEY_LEFTSHIFT, SPACE, KEY_TAB] {
        assert_eq!(
            kb.key_char(code),
            None,
            "keycode {code} 应是功能键（无字符）"
        );
    }
}
