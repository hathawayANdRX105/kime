//! Main 追加缺陷 A：成对标点没有闭合状态。
//! 契约（对齐 rime/fcitx5）：`"` 连按两次 → `“` 然后 `”`；中间遇到任何其它输入重置回开引号；
//! 英文标点模式原样透传；两条路径都不碰中英文模式。

use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::{Config, PunctMode};
use kime_core::dict::Dict;
use kime_core::{Engine, Key, Outcome};

fn engine(punct_mode: PunctMode) -> Engine {
    let db = std::env::temp_dir().join(format!(
        "kime_quote_{}_{}.sqlite",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&db);
    let dict = Dict::open(&db).unwrap();
    Engine::new(
        dict,
        Config {
            shuangpin: None,
            punct_mode,
            ..Config::default()
        },
    )
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

fn commit(e: &mut Engine, c: char) -> String {
    match press(e, c) {
        Outcome::Commit(t) => t,
        other => panic!("键 {c:?} 应 Commit，实际 {other:?}"),
    }
}

#[test]
fn double_quote_alternates_open_then_close() {
    let mut e = engine(PunctMode::Chinese);
    assert_eq!(commit(&mut e, '"'), "“", "第一次按 \" 出开引号");
    assert_eq!(
        commit(&mut e, '"'),
        "”",
        "连按第二次必须出闭引号（用户报的死循环点）"
    );
    assert_eq!(commit(&mut e, '"'), "“", "闭后再开，交替持续");
    assert!(e.chinese(), "标点不得切换中英文模式");
}

#[test]
fn single_quote_alternates_open_then_close() {
    let mut e = engine(PunctMode::Chinese);
    assert_eq!(commit(&mut e, '\''), "‘");
    assert_eq!(commit(&mut e, '\''), "’");
}

#[test]
fn any_other_key_resets_the_quote_state() {
    let mut e = engine(PunctMode::Chinese);
    assert_eq!(commit(&mut e, '"'), "“");
    // 字母进入组合（Consumed）并重置引号态；下一枚引号走「原串上屏并粘贴」路径，
    // 出的仍是开引号——状态没有停留在「待闭」。
    assert_eq!(press(&mut e, 'a'), Outcome::Consumed);
    assert_eq!(
        commit(&mut e, '"'),
        "a“",
        "中间打了字，下一次引号必须重新开"
    );
    // ESC 也重置；另一种引号不共享开闭状态
    e.key(Key {
        ch: None,
        code: 1,
        shift: false,
        ctrl: false,
        alt: false,
    });
    assert_eq!(commit(&mut e, '"'), "“", "ESC 之后重开");
    assert_eq!(commit(&mut e, '\''), "‘");
    assert_eq!(commit(&mut e, '"'), "“");
}

#[test]
fn rime_half_shape_deviations_are_fixed() {
    // Main 定案的 5 项偏差里除引号外的映射项（引号见上面交替测试）。
    let mut e = engine(PunctMode::Chinese);
    assert_eq!(commit(&mut e, '~'), "~", "rime half_shape: ~ 是半角");
    assert_eq!(commit(&mut e, '{'), "「");
    assert_eq!(commit(&mut e, '}'), "」");
    assert_eq!(commit(&mut e, '`'), "·");
    assert_eq!(commit(&mut e, '$'), "¥");
}

#[test]
fn english_punct_mode_passes_everything_through() {
    let mut e = engine(PunctMode::English);
    assert_eq!(commit(&mut e, '"'), "\"");
    assert_eq!(
        commit(&mut e, '"'),
        "\"",
        "英文标点模式下永远原样，不做开闭状态"
    );
    for c in ['~', '{', '`', '$'] {
        assert_eq!(
            commit(&mut e, c),
            c.to_string(),
            "英文标点模式必须原样 {c:?}"
        );
    }
    assert!(e.chinese());
}
