//! xkb 解码正确性的离线证明（工单第 2 条）。
//!
//! 真机路径是合成器 keymap 事件的 fd → 文本 → `Keyboard::set_keymap`；这里把
//! 同一份 XKB V1 文本直接喂进同一个函数，断言 shift 层与老手写码表漏掉的键位。
//! 键图来源：系统 xkb 数据编译 us/pc105 后 `get_as_string` 回文本（与真机送来的
//! 东西同格式）。唯一只能真机验的是「合成器确实把这份协议的 modifiers 掩码按
//! wl 约定发来」——bit0=shift 的换名逻辑在 keyboard.rs 里，本文件用同一入口测。

use platform_wayland::keyboard::Keyboard;
use xkbcommon::xkb;

/// 键图文本是真机 fd 的同格式产物；抽出来给 wl_bit 复用（同一份键图取 mod 位）。
fn compiled_us() -> xkb::Keymap {
    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    xkb::Keymap::new_from_names(
        &ctx,
        "evdev",
        "pc105",
        "us",
        "",
        None,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .expect("系统 xkb 数据不可用：编译 us 键图失败")
}

fn us_keyboard() -> Keyboard {
    let text = compiled_us().get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
    let mut kb = Keyboard::new();
    assert!(
        kb.set_keymap(&text),
        "xkb 自己产出的文本必须能被 set_keymap 装载"
    );
    kb
}

/// 修饰名 → wl_keyboard.modifiers 掩码位（bit 位 = 键图 mod 序号；虚拟 mod
/// 序号 ≥ 8 同样按位编码）。MOD_INVALID = 键图没这个修饰，测试前提不成立直接红。
fn wl_bit(name: &str) -> u32 {
    let idx = compiled_us().mod_get_index(name);
    assert_ne!(idx, xkb::MOD_INVALID, "us 键图缺修饰 {name}");
    1u32 << idx
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

/// evdev：b/f/h，Ctrl 兜底回归用。
const KEY_B: u32 = 48;
const KEY_F: u32 = 33;
const KEY_H: u32 = 35;

/// wl 标准键图 real mod 序号：bit2 = Control。
const WL_CTRL: u32 = 1 << 2;

#[test]
fn ctrl_held_letters_decode_via_keysym_fallback() {
    // 第五轮根因：libxkbcommon 的 key_get_utf32 在 Control 激活时做 XkbToControl
    // （'f'→0x06），旧 key_char 把它当控制字符过滤成 None → 引擎 C-b/f/h/n/p/.
    // 分支永不可达（真机不生效、引擎单测却全绿）。兜底改走 key_get_one_sym。
    let mut kb = us_keyboard();
    assert_eq!(kb.key_char(KEY_F), Some('f'), "无 Ctrl 时基线不变");
    kb.update_mods(WL_CTRL, 0, 0, 0);
    assert_eq!(kb.key_char(KEY_F), Some('f'), "Ctrl+f 必须解出 f 喂引擎");
    assert_eq!(kb.key_char(KEY_H), Some('h'), "Ctrl+h 必须解出 h");
    assert_eq!(kb.key_char(KEY_B), Some('b'), "Ctrl+b 必须解出 b");
    // Ctrl+. 标点模式切换：'.'(0x2e) 不被 XkbToControl 打断，主路径直出——钉住防回归。
    assert_eq!(kb.key_char(KEY_PERIOD), Some('.'), "Ctrl+. 解出句点");
    // Shift 与 Ctrl 同按时字母按 shift 层解（大写 F），引擎 ctrl 组合仍命中它的路径。
    kb.update_mods(WL_CTRL | WL_SHIFT, 0, 0, 0);
    assert_eq!(kb.key_char(KEY_F), Some('F'), "Ctrl+Shift+f 解出大写 F");
}

#[test]
fn ctrl_held_non_letter_combos_stay_charless() {
    // 白名单只放行 ASCII 字母 + '.'。Ctrl 把下面这些键的 utf32 打成控制字符
    // （'2'→0x00、'3'→0x1b、'/'→0x1f、Tab/Enter/Esc/Space/BS→0x09/0d/1b/00/08），
    // 主路径已 None；若兜底不加白名单，key_get_one_sym 会把 '2'/'3'/'/' 重新解出
    // ——那样浏览器 Ctrl+2 切标签、Ctrl+/ 看快捷键都会被引擎误吞。白名单必须挡住。
    let mut kb = us_keyboard();
    kb.update_mods(WL_CTRL, 0, 0, 0);
    const KEY_2: u32 = 3;
    const KEY_3: u32 = 4;
    const KEY_LEFTBRACE: u32 = 26;
    const KEY_BACKSPACE: u32 = 14;
    for code in [
        KEY_2,
        KEY_3,
        KEY_SLASH,
        KEY_LEFTBRACE,
        KEY_ENTER,
        KEY_TAB,
        SPACE,
        KEY_ESC,
        KEY_BACKSPACE,
    ] {
        assert_eq!(kb.key_char(code), None, "Ctrl+{code} 不该被兜底解成字符");
    }
}

#[test]
fn ctrl_held_printable_survivors_go_through_primary_path() {
    // 反例锁定：libxkbcommon 的 XkbToControl 只对 >=0x40 生效，'1'/'9'/'0'/','/';' 在
    // Ctrl 下 utf32 未被打断 → 主路径直接给字符，与我的兜底白名单无关。这是既有行为
    // （改动前后 key_char 对这些键返回一致），钉住以防兜底误改主路径。
    let mut kb = us_keyboard();
    kb.update_mods(WL_CTRL, 0, 0, 0);
    assert_eq!(kb.key_char(KEY_1), Some('1'));
    assert_eq!(kb.key_char(KEY_9), Some('9'));
    assert_eq!(kb.key_char(KEY_COMMA), Some(','));
    assert_eq!(kb.key_char(KEY_SEMICOLON), Some(';'));
    // Ctrl+字母全解出：兜底没误伤引擎其余 ctrl 组合（C-n/C-p 翻页等）。
    assert_eq!(kb.key_char(49 /* n */), Some('n'));
    assert_eq!(kb.key_char(25 /* p */), Some('p'));
}

#[test]
fn update_mods_ctrl_mask_derives_ctrl_flag() {
    // 旗标权威来源是合成器 Modifiers 掩码（虚拟键盘/grab 重建后键码记账缺失，
    // 只有掩码可信）：掩码到位 ctrl() 必须亮，清零必须灭。
    let mut kb = us_keyboard();
    assert!(!kb.ctrl(), "初始无修饰旗标必须为灭");
    kb.update_mods(WL_CTRL, 0, 0, 0);
    assert!(kb.ctrl(), "Control 位激活必须推导出 ctrl 旗标");
    kb.update_mods(0, 0, 0, 0);
    assert!(!kb.ctrl(), "掩码清零必须灭旗标");
}

#[test]
fn update_mods_alt_mask_derives_alt_flag() {
    // Alt 在 us/pc105 是虚拟 mod（序号 ≥ 8）：按「位 = mod 序号」喂掩码，
    // 证明推导不依赖 Control/Shift 所在的 real mod 区（0..7）。
    let mut kb = us_keyboard();
    let alt_bit = wl_bit("Alt");
    kb.update_mods(alt_bit, 0, 0, 0);
    assert!(kb.alt(), "Alt 位激活必须推导出 alt 旗标");
    assert!(!kb.ctrl(), "Alt 激活不得误亮 ctrl");
    kb.update_mods(0, 0, 0, 0);
    assert!(!kb.alt(), "掩码清零必须灭 alt 旗标");
}

#[test]
fn note_key_sniff_sets_flags_between_modifiers_frames() {
    // 键码嗅探（main.rs Key 分支同构）只填两帧 Modifiers 之间的空档：按下亮、
    // 抬起灭；左右 Ctrl 码（29|97）与 Alt 码（56|100）都认，普通键是 no-op。
    let mut kb = us_keyboard();
    kb.note_key(29, true);
    assert!(kb.ctrl(), "左 Ctrl 按下（无 Modifiers 帧）旗标须亮");
    kb.note_key(29, false);
    assert!(!kb.ctrl(), "左 Ctrl 抬起须灭");
    kb.note_key(97, true);
    assert!(kb.ctrl(), "右 Ctrl 按下同样点亮");
    kb.note_key(97, false);
    assert!(!kb.ctrl(), "右 Ctrl 抬起须灭");
    kb.note_key(56, true);
    assert!(kb.alt(), "左 Alt 按下点亮 alt");
    kb.note_key(56, false);
    assert!(!kb.alt(), "左 Alt 抬起须灭");
    kb.note_key(100, true);
    assert!(kb.alt(), "右 Alt 按下同样点亮");
    kb.note_key(100, false);
    assert!(!kb.alt(), "右 Alt 抬起须灭");
    kb.note_key(KEY_A, true);
    assert!(!kb.ctrl() && !kb.alt(), "普通字母键不得误触旗标");
}

#[test]
fn wtype_mods_without_ctrl_keycode_still_derives_flag() {
    // wtype 虚拟键盘从不发 29|97 物理键：Modifiers 带 Control 位 + 普通字母 Key。
    // 旧实现只靠键码记账 → 旗标恒 false → ctrl+c 被当拼音吞；钉住掩码路径。
    let mut kb = us_keyboard();
    kb.note_key(KEY_A, true);
    assert!(!kb.ctrl(), "普通字母 Key 不得凭空点亮 ctrl");
    kb.update_mods(WL_CTRL, 0, 0, 0);
    assert!(kb.ctrl(), "仅凭掩码（无 29|97 键码）必须推导出 ctrl");
    kb.update_mods(0, 0, 0, 0);
    assert!(!kb.ctrl(), "掩码清零灭旗标");
}
