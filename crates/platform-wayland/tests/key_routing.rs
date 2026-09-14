//! 壳层按键路由的集成测试（第五轮工单第 4 条）。
//!
//! 引擎单测绿而真机 C-f/C-b/C-h 不生效，缺的就是这一层：key_char（真 xkb 解码，
//! 含 Ctrl 的 XkbToControl 变换）→ shell_key 构造 → engine.key → route_press →
//! swallow/repeat 记账。这里用系统 us 键图 + 临时词库把整条链原样跑一遍；
//! wayland 连接不存在，转发/上屏动作以记账断言。keyboard.rs / route.rs 任何一方
//! 回退成旧实现，下面的 ctrl_* 用例立刻红。

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::{Engine, Outcome};
use platform_wayland::keyboard::Keyboard;
use platform_wayland::repeat::{KeyRepeat, REPEAT_DELAY_MS, REPEAT_INTERVAL_MS};
use platform_wayland::route::{key_log_line, route_press, route_release, shell_key, PressAction};
use platform_wayland::SwallowTracker;
use xkbcommon::xkb;

// evdev keycode（linux/input-event-codes.h）
const KEY_CTRL: u32 = 29;
const KEY_BACKSPACE: u32 = 14;
const KEY_B: u32 = 48;
const KEY_F: u32 = 33;
const KEY_H: u32 = 35;
const KEY_N: u32 = 49;
const KEY_PERIOD: u32 = 52;
const KEY_2: u32 = 3;

/// 壳层假表原点（main.rs 用 CLOCK_MONOTONIC now_ms；harness 把时间捏在手里）。
const T0: u64 = 1_000_000_000;

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
    assert!(kb.set_keymap(&text));
    kb
}

/// 真 Control mod 位：从同一份键图取，不写死 bit2（协议约定 = real mod 序号）。
fn wl_ctrl_bit() -> u32 {
    1u32 << compiled_us().mod_get_index("Control")
}

/// 词库 fixture（对齐 kime-core english_layer_one_test：`...` 正文分隔线 +
/// 词\t拼音\t词频；Dict::import 只吃分隔线之后的行，缺它导入 0 条）。
const CN: &str = "\
...
你好\tni hao\t9000000
拟好\tni hao\t4000000
你\tni\t6000000
好\thao\t5000000
安\tan\t8000000
";

/// 复刻 dispatch + engine_press 的壳层 harness：真 Keyboard（xkb 解码含 ctrl 变换）、
/// 真 Engine、真 SwallowTracker/KeyRepeat，唯一假的是 wayland——转发/上屏记成账。
struct Shell {
    keyboard: Keyboard,
    ctrl_bit: u32,
    engine: Engine,
    swallowed: SwallowTracker,
    repeat: KeyRepeat,
    ctrl: bool,
    alt: bool,
    /// vk.key 观测：(code, pressed)
    forwarded: Vec<(u32, bool)>,
    /// commit_string 观测
    commits: Vec<String>,
}

impl Shell {
    fn new(tag: &str) -> (Self, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "kime_route_{}_{}_{}",
            tag,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let cn = dir.join("cn.tsv");
        fs::write(&cn, CN).unwrap();
        let mut dict = Dict::open(dir.join("dict.sqlite3")).unwrap();
        dict.import(&cn).unwrap();
        let engine = Engine::new(dict, Config::default());
        (
            Self {
                keyboard: us_keyboard(),
                ctrl_bit: wl_ctrl_bit(),
                engine,
                swallowed: SwallowTracker::default(),
                repeat: KeyRepeat::default(),
                ctrl: false,
                alt: false,
                forwarded: Vec::new(),
                commits: Vec::new(),
            },
            dir,
        )
    }

    /// 真 press：dispatch 对 29|97/56|100 的旗标记账 + 合成器 Modifiers 事件
    /// （grab Key 旁必有一条，两步并成一步）+ engine_press。
    fn press(&mut self, code: u32) -> Outcome {
        self.set_modifier(code, true);
        self.engine_press(code, false)
    }

    /// 真 release：route_release 决定吞放，放行的记进 forwarded。
    fn release(&mut self, code: u32) {
        self.set_modifier(code, false);
        if route_release(code, &mut self.swallowed, &mut self.repeat) {
            self.forwarded.push((code, false));
        }
    }

    fn set_modifier(&mut self, code: u32, down: bool) {
        match code {
            KEY_CTRL | 97 => {
                self.ctrl = down;
                self.keyboard
                    .update_mods(if down { self.ctrl_bit } else { 0 }, 0, 0, 0);
            }
            56 | 100 => self.alt = down,
            _ => {}
        }
    }

    /// main.rs engine_press 的同构复刻（now_ms 换成假表 T0，qh 副作用换成账目）。
    fn engine_press(&mut self, code: u32, synthetic: bool) -> Outcome {
        let ch = self.keyboard.key_char(code);
        let (ctrl, alt) = (self.ctrl, self.alt);
        let outcome = self.engine.key(shell_key(code, ch, ctrl, alt));
        let composing = !self.engine.preedit().is_empty();
        let route = route_press(code, synthetic, composing, &outcome);
        if route.disarm {
            self.repeat.release(code);
        }
        if route.arm {
            self.repeat.arm(code, T0);
        }
        match route.action {
            PressAction::Consume => {
                self.swallowed.consume(code);
                if let Outcome::Commit(text) = outcome.clone() {
                    self.commits.push(text);
                }
            }
            PressAction::Forward => {
                self.swallowed.forward(code);
                self.forwarded.push((code, true));
            }
            PressAction::Tap => {
                self.forwarded.push((code, true));
                self.forwarded.push((code, false));
                self.swallowed.consume(code);
            }
        }
        outcome
    }

    /// 主循环 poll 到期唤醒：合成 tick 走 synthetic press。
    fn tick(&mut self, now: u64) -> Option<u32> {
        let code = self.repeat.tick(now)?;
        self.engine_press(code, true);
        Some(code)
    }

    fn type_str(&mut self, s: &str) {
        for c in s.chars() {
            let code = letter_code(c);
            assert_eq!(self.press(code), Outcome::Consumed, "字母 {c} 应被消费");
        }
    }
}

/// QWERTY 字母 → evdev 码（键盘行序不是字母序，写死表最笨也最不容易错）。
fn letter_code(c: char) -> u32 {
    match c {
        'a' => 30,
        'b' => 48,
        'c' => 46,
        'd' => 32,
        'e' => 18,
        'f' => 33,
        'g' => 34,
        'h' => 35,
        'i' => 23,
        'j' => 36,
        'k' => 37,
        'l' => 38,
        'm' => 50,
        'n' => 49,
        'o' => 24,
        'p' => 25,
        'q' => 16,
        'r' => 19,
        's' => 31,
        't' => 20,
        'u' => 22,
        'v' => 47,
        'w' => 17,
        'x' => 45,
        'y' => 21,
        'z' => 44,
        other => panic!("type_str 只吃 a-z: {other}"),
    }
}

/// ① 空组合：Ctrl+f 全序列原样放行给应用（应用的 emacs 移动/Ctrl+F 查找照旧）。
#[test]
fn empty_composition_ctrl_f_reaches_app() {
    let (mut sh, dir) = Shell::new("empty_cf");
    assert_eq!(sh.press(KEY_CTRL), Outcome::Ignored, "Ctrl 自身放行");
    assert_eq!(sh.press(KEY_F), Outcome::Ignored, "空组合 C-f 不吞键");
    sh.release(KEY_F);
    sh.release(KEY_CTRL);
    assert_eq!(
        sh.forwarded,
        vec![
            (KEY_CTRL, true),
            (KEY_F, true),
            (KEY_F, false),
            (KEY_CTRL, false)
        ],
        "应用必须看到完整按下抬起，一个不少一个不多"
    );
    fs::remove_dir_all(dir).unwrap();
}

/// ② 组合内 Ctrl+f/Ctrl+b：xkb 真解码出字符、引擎消费、press/release 不转发。
/// 这就是藏了几轮的回归本体：旧 key_char 在 Ctrl 下给 ch=None → Ignored → 转发。
#[test]
fn composition_ctrl_f_b_consumed_not_forwarded() {
    let (mut sh, dir) = Shell::new("comp_cf");
    sh.type_str("nihao");
    assert_eq!(sh.engine.cursor(), 5, "打字后光标贴尾");
    sh.press(KEY_CTRL);
    assert_eq!(sh.press(KEY_B), Outcome::Consumed);
    assert_eq!(
        sh.engine.cursor(),
        4,
        "C-b 左移一格（真 xkb：ch=Some('b')）"
    );
    assert_eq!(sh.press(KEY_F), Outcome::Consumed);
    assert_eq!(sh.engine.cursor(), 5, "C-f 右移回尾");
    sh.release(KEY_B);
    sh.release(KEY_F);
    sh.release(KEY_CTRL);
    assert_eq!(
        sh.forwarded,
        vec![(KEY_CTRL, true), (KEY_CTRL, false)],
        "组合内 C-f/C-b 不许转发；只有 Ctrl 自身放行且配对完整"
    );
    fs::remove_dir_all(dir).unwrap();
}

/// ③ 组合内 Ctrl+h 逐格回删；删空组合后落回 Ignored——组合外删除归应用。
#[test]
fn composition_ctrl_h_deletes_then_empty_ignores() {
    let (mut sh, dir) = Shell::new("comp_ch");
    sh.type_str("nihao");
    assert_eq!(sh.press(KEY_CTRL), Outcome::Ignored);
    for expect in [4, 3, 2, 1, 0] {
        assert_eq!(sh.press(KEY_H), Outcome::Consumed, "删到 cursor={expect}");
        assert_eq!(sh.engine.cursor(), expect);
    }
    assert!(sh.engine.preedit().is_empty(), "五次 C-h 删空组合");
    assert_eq!(sh.press(KEY_H), Outcome::Ignored, "空组合 C-h 放行给应用");
    sh.release(KEY_H);
    sh.release(KEY_CTRL);
    assert!(
        sh.forwarded.contains(&(KEY_H, true)) && sh.forwarded.contains(&(KEY_H, false)),
        "空组合那下 C-h 的 press+release 都得还给应用: {:?}",
        sh.forwarded
    );
    let h_forwards = sh.forwarded.iter().filter(|(c, _)| *c == KEY_H).count();
    assert_eq!(
        h_forwards, 2,
        "组合内五次消费绝不出现在 forwarded: {:?}",
        sh.forwarded
    );
    fs::remove_dir_all(dir).unwrap();
}

/// 组合内、光标在头、组合非空：C-h 消费且什么都不删——rime 静默语义。
/// 依据：librime editor.cc::BackToPreviousInput/BackToPreviousSyllable 无条件
/// return true（组合在场即吞键），context.cc::PopInput 在 caret_pos==0 返回 false
/// （不删）。组合头按 Backspace 不会去删 preedit 外的应用文本——放行反而偏离 rime。
#[test]
fn ctrl_h_at_head_mid_composition_swallows_silently() {
    let (mut sh, dir) = Shell::new("ch_head");
    sh.type_str("nihao");
    sh.press(KEY_CTRL);
    for _ in 0..5 {
        sh.press(KEY_B);
    }
    assert_eq!(sh.engine.cursor(), 0);
    let pre = sh.engine.preedit().to_string();
    assert!(!pre.is_empty());
    assert_eq!(sh.press(KEY_H), Outcome::Consumed, "组合头 C-h：消费");
    assert_eq!(sh.engine.preedit(), pre, "但什么都不删");
    sh.release(KEY_H);
    sh.release(KEY_CTRL);
    assert!(
        !sh.forwarded.contains(&(KEY_H, true)),
        "消费掉的 C-h 不许转发: {:?}",
        sh.forwarded
    );
    fs::remove_dir_all(dir).unwrap();
}

/// ④ 组合内长按 C-b：真 press 起表 → 合成 tick 消费移动 → 物理 release 撤表停拍。
#[test]
fn hold_ctrl_b_arms_repeat_ticks_move_then_release_disarms() {
    let (mut sh, dir) = Shell::new("hold_cb");
    sh.type_str("nihao");
    sh.press(KEY_CTRL);
    assert_eq!(sh.press(KEY_B), Outcome::Consumed);
    assert_eq!(sh.engine.cursor(), 4);
    assert_eq!(sh.repeat.next_due(), Some(T0 + REPEAT_DELAY_MS), "起表时刻");
    assert_eq!(sh.tick(T0 + REPEAT_DELAY_MS - 1), None, "未到点不合成");
    assert_eq!(sh.tick(T0 + REPEAT_DELAY_MS), Some(KEY_B));
    assert_eq!(sh.engine.cursor(), 3, "第一拍：再左移");
    assert_eq!(
        sh.tick(T0 + REPEAT_DELAY_MS + REPEAT_INTERVAL_MS),
        Some(KEY_B)
    );
    assert_eq!(sh.engine.cursor(), 2);
    sh.release(KEY_B);
    assert_eq!(sh.repeat.next_due(), None, "物理 release 撤表");
    assert_eq!(sh.tick(T0 + 9999), None, "撤表后不再合成");
    sh.release(KEY_CTRL);
    assert_eq!(
        sh.forwarded,
        vec![(KEY_CTRL, true), (KEY_CTRL, false)],
        "整段只有 Ctrl 本身放行"
    );
    fs::remove_dir_all(dir).unwrap();
}

/// 合成 tick 撞上组合被删空：给应用补一次完整点击（tap_pair 路由），
/// 物理 release 到时须被吞（点击已闭环）。
#[test]
fn synthetic_tick_on_emptied_composition_taps_app() {
    let (mut sh, dir) = Shell::new("tap");
    sh.type_str("ni");
    assert_eq!(sh.press(KEY_BACKSPACE), Outcome::Consumed);
    assert_eq!(sh.engine.cursor(), 1, "删到剩 1 字母，组合还在 → 起表");
    assert_eq!(sh.repeat.next_due(), Some(T0 + REPEAT_DELAY_MS));
    assert_eq!(sh.tick(T0 + REPEAT_DELAY_MS), Some(KEY_BACKSPACE));
    assert!(sh.engine.preedit().is_empty(), "这一拍删空组合");
    sh.tick(T0 + REPEAT_DELAY_MS + REPEAT_INTERVAL_MS);
    assert_eq!(
        sh.forwarded,
        vec![(KEY_BACKSPACE, true), (KEY_BACKSPACE, false)],
        "组合空后的下一拍 = 还给应用一次完整退格点击"
    );
    sh.release(KEY_BACKSPACE);
    assert_eq!(
        sh.forwarded.len(),
        2,
        "tap 闭环后物理 release 必须被吞（记账不新增）: {:?}",
        sh.forwarded
    );
    fs::remove_dir_all(dir).unwrap();
}

/// ⑤ Ctrl 按住时字母键的 Key 字段正确性 + C-n 翻页路由复活（引擎入参级钉桩）。
#[test]
fn ctrl_held_key_fields_match_engine_contract() {
    let (mut sh, dir) = Shell::new("key_fields");
    sh.type_str("ni"); // 先建立组合（按住 Ctrl 打字会被引擎 Ignored，属另一条契约）
    sh.press(KEY_CTRL);
    let key = shell_key(KEY_N, sh.keyboard.key_char(KEY_N), sh.ctrl, sh.alt);
    assert_eq!(key.code, KEY_N);
    assert_eq!(key.ch, Some('n'), "真 xkb 下 Ctrl+N 必须带字符进引擎");
    assert!(key.ctrl && !key.shift && !key.alt);
    assert_eq!(
        sh.engine_press(KEY_N, false),
        Outcome::Consumed,
        "有候选时 C-n 被引擎翻页消费（旧解码下这步永远走不到）"
    );
    fs::remove_dir_all(dir).unwrap();
}

/// 白名单边界：Ctrl+数字照旧放行（浏览器 Ctrl+2 不被选词吞），
/// Ctrl+. 则被引擎标点切换消费。
#[test]
fn ctrl_digit_forwarded_ctrl_dot_consumed() {
    let (mut sh, dir) = Shell::new("cfwd");
    sh.type_str("ni");
    sh.press(KEY_CTRL);
    assert_eq!(sh.press(KEY_2), Outcome::Ignored, "Ctrl+2 不选词");
    sh.release(KEY_2);
    assert_eq!(
        sh.press(KEY_PERIOD),
        Outcome::Consumed,
        "C-. = 标点模式切换"
    );
    sh.release(KEY_PERIOD);
    sh.release(KEY_CTRL);
    assert!(
        sh.forwarded.contains(&(KEY_2, true)) && sh.forwarded.contains(&(KEY_2, false)),
        "Ctrl+2 完整放行: {:?}",
        sh.forwarded
    );
    assert!(
        !sh.forwarded.contains(&(KEY_PERIOD, true)),
        "C-. 被消费不许转发: {:?}",
        sh.forwarded
    );
    fs::remove_dir_all(dir).unwrap();
}

/// Contract 日志行格式钉桩：`key code=<u32> ch=<char|-> mods=<c?><s?><a?> -> <Outcome>`
#[test]
fn key_log_line_contract_format() {
    assert_eq!(
        key_log_line(KEY_F, Some('f'), true, false, false, &Outcome::Consumed),
        "key code=33 ch=f mods=c-- -> Consumed\n"
    );
    assert_eq!(
        key_log_line(KEY_F, None, true, false, false, &Outcome::Ignored),
        "key code=33 ch=- mods=c-- -> Ignored\n"
    );
    assert_eq!(
        key_log_line(
            KEY_H,
            Some('h'),
            true,
            true,
            true,
            &Outcome::Commit("你好".into())
        ),
        "key code=35 ch=h mods=csa -> Commit(你好)\n"
    );
}
