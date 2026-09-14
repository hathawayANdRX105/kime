//! 标点路由契约（rime-ice half_shape 已全量 32 条目入表，见 punct_rime_parity_test.rs）：
//!
//! 1. **恒等映射条目**（rime 里 `/ | @ # % & * - + =` 映射为自身）走 map_punct 映射分支：
//!    组合态顶字上屏、原字符续后（与全角标点情况 B/C 同构）；无组合时 Commit 该半角字符。
//!    屏面结果与宿主透传一致，只是上屏路径归引擎。
//! 2. **Ctrl/Alt 组合键放行**：rime punctuator 不做带修饰键的标点映射，
//!    Ctrl+- / Ctrl+= / Ctrl+/ 等组合直达宿主。
//! 3. **digit_separators**（rime `,:.` 同款）：数字后紧跟的 ` , . : ` 跳过映射直通。

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::{Engine, Key, Outcome};

const CN: &str = "\
...
你	ni	9000000
泥	ni	100
";

fn engine() -> (Engine, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "kime_punct_route_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let yaml = dir.join("cn.yaml");
    fs::write(&yaml, CN).unwrap();
    let mut dict = Dict::open(dir.join("dict.sqlite3")).unwrap();
    dict.import(&yaml).unwrap();
    let e = Engine::new(
        dict,
        Config {
            shuangpin: None,
            ..Config::default()
        },
    );
    (e, dir)
}

fn press(e: &mut Engine, c: char) -> Outcome {
    e.key(Key {
        ch: Some(c),
        code: 0,
        shift: false,
        ctrl: false,
        alt: false,
    })
}

fn type_letters(e: &mut Engine, s: &str) {
    for c in s.chars() {
        assert_eq!(press(e, c), Outcome::Consumed);
    }
}

/// 组合态 + 有候选：恒等映射的 `@` 顶字上屏，符号原样续后。
#[test]
fn identity_punct_commits_top_candidate() {
    let (mut e, dir) = engine();
    type_letters(&mut e, "ni");
    assert_eq!(press(&mut e, '@'), Outcome::Commit("你@".into()));
    assert!(e.preedit().is_empty(), "顶字后组合必须清空");
    fs::remove_dir_all(dir).unwrap();
}

/// 组合态 + 无候选（解不出的键串）：原样字母 + 符号上屏，与映射标点情况 C 同构。
#[test]
fn identity_punct_commits_raw_letters_without_candidates() {
    let (mut e, dir) = engine();
    type_letters(&mut e, "xq");
    assert!(e.candidates().is_empty(), "种子词库下 xq 应无候选");
    assert_eq!(press(&mut e, '#'), Outcome::Commit("xq#".into()));
    fs::remove_dir_all(dir).unwrap();
}

/// 无组合：恒等符号 Commit 自身（rime half_shape 直接上屏，屏面等价于透传）。
#[test]
fn identity_punct_without_composition_commits_self() {
    let (mut e, dir) = engine();
    assert_eq!(press(&mut e, '@'), Outcome::Commit("@".into()));
    assert_eq!(press(&mut e, '/'), Outcome::Commit("/".into()));
    fs::remove_dir_all(dir).unwrap();
}

/// Ctrl/Alt + 符号：punctuator 不映射带修饰键的组合，一律放行宿主
/// （Ctrl+- / Ctrl+= 是浏览器缩放、Ctrl+/ 是应用搜索，绝不能被吞）。
#[test]
fn ctrl_alt_symbol_chords_pass_through() {
    let (mut e, dir) = engine();
    type_letters(&mut e, "ni");
    let mut chord = Key {
        ch: Some('#'),
        code: 0,
        shift: false,
        ctrl: false,
        alt: false,
    };
    chord.ctrl = true;
    assert_eq!(
        e.key(chord),
        Outcome::Ignored,
        "Ctrl+符号放行宿主，组合不被顶字路径吞掉"
    );
    chord.ctrl = false;
    chord.alt = true;
    assert_eq!(e.key(chord), Outcome::Ignored, "Alt 同理");
    // Ctrl+`,`（映射键同样受守卫保护，此前是吞键的既有 bug）
    chord.alt = false;
    chord.ctrl = true;
    chord.ch = Some(',');
    assert_eq!(e.key(chord), Outcome::Ignored, "Ctrl+, 不再被吞为 ，");
    fs::remove_dir_all(dir).unwrap();
}

/// digit_separators：`23.8` 全程逐键断言 —— 数字放行、小数点直通半角。
#[test]
fn dot_after_digit_stays_halfwidth() {
    let (mut e, dir) = engine();
    assert_eq!(press(&mut e, '2'), Outcome::Ignored);
    assert_eq!(press(&mut e, '3'), Outcome::Ignored);
    assert_eq!(
        press(&mut e, '.'),
        Outcome::Ignored,
        "数字后的 . 必须直通半角（旧行为：Commit(\"。\")）"
    );
    assert_eq!(press(&mut e, '8'), Outcome::Ignored);
    fs::remove_dir_all(dir).unwrap();
}

/// 逗号/冒号同规则；标志只活一跳：数字后第一个 ` , ` 直通，之后任何非数字键清零，
/// 再按 ` , ` 恢复全角顶格上屏。
#[test]
fn comma_and_colon_after_digit() {
    let (mut e, dir) = engine();
    assert_eq!(press(&mut e, '5'), Outcome::Ignored);
    assert_eq!(press(&mut e, ','), Outcome::Ignored, "数字后 , 半角直通");
    assert_eq!(
        press(&mut e, ','),
        Outcome::Commit("，".into()),
        "上一键已是标点，标志清零，恢复全角"
    );
    assert_eq!(press(&mut e, ':'), Outcome::Commit("：".into()));
    assert_eq!(press(&mut e, '9'), Outcome::Ignored);
    assert_eq!(press(&mut e, ':'), Outcome::Ignored, "数字后 : 半角直通");
    fs::remove_dir_all(dir).unwrap();
}

/// 非数字之后行为不变：无组合 `.` 上屏全角句号；有组合顶字 + 全角。
#[test]
fn punct_mapping_outside_digit_context_unchanged() {
    let (mut e, dir) = engine();
    assert_eq!(press(&mut e, '.'), Outcome::Commit("。".into()));
    type_letters(&mut e, "ni");
    assert_eq!(press(&mut e, '.'), Outcome::Commit("你。".into()));
    fs::remove_dir_all(dir).unwrap();
}

/// 「数字键无论 outcome 置位」：`1` 选词上屏（Commit 而非 Ignored）之后，
/// 紧跟的 `.` 仍必须直通半角 —— 打「你好。1.5」这类混排不乱吃全角。
#[test]
fn digit_separator_after_selection_commit() {
    let (mut e, dir) = engine();
    type_letters(&mut e, "ni");
    assert_eq!(press(&mut e, '1'), Outcome::Commit("你".into()));
    assert_eq!(
        press(&mut e, '.'),
        Outcome::Ignored,
        "选词提交后仍是「数字之后」，. 直通"
    );
    fs::remove_dir_all(dir).unwrap();
}
