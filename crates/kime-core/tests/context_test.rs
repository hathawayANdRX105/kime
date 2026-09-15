//! 上下文通道（第六轮①）：surrounding_text 归一化 + engine 上下文存取。
//!
//! 三件事：
//! 1. [`ContextTail::from_surrounding`] 的边界语义（中英混排、多字节截断）；
//! 2. [`Engine::set_context`] / [`Engine::context_tail`] 往返；
//! 3. 上下文与组合生命周期无关 —— commit / Esc 清组合后上下文仍在。

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::{ContextTail, Engine, Key, Outcome};

fn tmp_db() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "kime_context_{}_{}.sqlite",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn engine(db: &std::path::Path) -> Engine {
    // 上下文通道不查词库，空库足够；schema 必须建。
    let _ = Dict::open(db).expect("create schema");
    Engine::new(
        Dict::open(db).expect("reopen dict"),
        Config {
            shuangpin: None,
            ..Config::default()
        },
    )
}

fn ch(c: char) -> Key {
    Key {
        ch: Some(c),
        code: 0,
        shift: false,
        ctrl: false,
        alt: false,
    }
}

#[test]
fn context_tail_truncates_on_char_boundary() {
    // 「我们在这」12 字节；取末 2 字必须是「在这」，不是半个字符
    let t = ContextTail::from_surrounding("我们在这", 12).expect("valid cursor");
    assert_eq!(t.tail_before_cursor(2), Some("在这"));
    // 光标在「我们」之后（字节 6），取 1 字 → 「们」
    let t = ContextTail::from_surrounding("我们在这", 6).expect("valid cursor");
    assert_eq!(t.tail_before_cursor(1), Some("们"));
}

#[test]
fn context_tail_rejects_cursor_inside_char() {
    assert!(ContextTail::from_surrounding("你好", 1).is_none());
}

#[test]
fn engine_context_round_trip() {
    let db = tmp_db();
    let mut e = engine(&db);

    assert_eq!(e.context_tail(), None, "新建引擎无上下文");
    e.set_context(Some("我们".to_string()));
    assert_eq!(e.context_tail(), Some("我们"));
    // 覆盖而非追加
    e.set_context(Some("吃饭".to_string()));
    assert_eq!(e.context_tail(), Some("吃饭"));
    e.set_context(None);
    assert_eq!(e.context_tail(), None);
    let _ = fs::remove_file(db);
}

#[test]
fn context_survives_clearing_composition() {
    // 上下文与组合生命周期无关：打组合 → Esc 清组合 → 上下文必须仍在
    let db = tmp_db();
    let mut e = engine(&db);
    e.set_context(Some("我们".to_string()));

    e.key(ch('n'));
    e.key(ch('i'));
    assert!(!e.preedit().is_empty(), "已有组合");
    assert_eq!(
        e.key(Key {
            ch: None,
            code: 1, // KEY_ESC
            shift: false,
            ctrl: false,
            alt: false
        }),
        Outcome::Consumed
    );
    assert!(e.preedit().is_empty(), "组合已清空");
    assert_eq!(e.context_tail(), Some("我们"), "清组合不清上下文");
    let _ = fs::remove_file(db);
}

#[test]
fn from_surrounding_ascii_and_end_cursor() {
    let t = ContextTail::from_surrounding("hello", 5).expect("cursor at end");
    assert_eq!(t.text(), "hello");
    assert_eq!(t.cursor(), 5);
}

#[test]
fn from_surrounding_empty_text() {
    let t = ContextTail::from_surrounding("", 0).expect("empty text, cursor 0");
    assert_eq!(t.text(), "");
    assert_eq!(t.cursor(), 0);
}

#[test]
fn from_surrounding_cursor_inside_multibyte_char() {
    // 「你好」= 6 字节，每个字符 3 字节 → 1/2/4/5 都在字符中间
    for bad in [1usize, 2, 4, 5] {
        assert!(
            ContextTail::from_surrounding("你好", bad).is_none(),
            "cursor {bad} 必须落在字符边界上"
        );
    }
}

#[test]
fn from_surrounding_cursor_beyond_len() {
    assert!(ContextTail::from_surrounding("abc", 4).is_none());
}

#[test]
fn tail_before_cursor_chinese_two_chars() {
    // 「我们在这」= 12 字节；光标在末尾，取末 2 字 → 「在这」
    let t = ContextTail::from_surrounding("我们在这", 12).unwrap();
    assert_eq!(t.tail_before_cursor(2), Some("在这"));
}

#[test]
fn tail_before_cursor_counts_chars_not_bytes() {
    // 光标在「我们」之后（字节 6），取 1 个字符 → 「们」（不是半个「我」）
    let t = ContextTail::from_surrounding("我们在这", 6).unwrap();
    assert_eq!(t.tail_before_cursor(1), Some("们"));
    assert_eq!(t.tail_before_cursor(2), Some("我们"));
}

#[test]
fn tail_before_cursor_mixed_ascii_and_cjk() {
    // 混排：abc你好，光标在「你好」之后（字节 3+6=9）
    let t = ContextTail::from_surrounding("abc你好", 9).unwrap();
    assert_eq!(t.tail_before_cursor(2), Some("你好"));
    assert_eq!(t.tail_before_cursor(3), Some("c你好"));
}

#[test]
fn tail_before_cursor_max_zero_is_none() {
    let t = ContextTail::from_surrounding("你好", 6).unwrap();
    assert_eq!(t.tail_before_cursor(0), None);
}

#[test]
fn tail_before_cursor_at_start_is_none() {
    let t = ContextTail::from_surrounding("你好", 0).unwrap();
    assert_eq!(t.tail_before_cursor(4), None);
}

#[test]
fn tail_before_cursor_max_exceeds_available() {
    // 要 100 个字符，只有 4 个 → 全量返回，不报错不补齐
    let t = ContextTail::from_surrounding("我们在这", 12).unwrap();
    assert_eq!(t.tail_before_cursor(100), Some("我们在这"));
}

#[test]
fn is_from_input_method_only_zero() {
    assert!(kime_core::context::is_from_input_method(0));
    assert!(!kime_core::context::is_from_input_method(1));
    assert!(!kime_core::context::is_from_input_method(2));
    assert!(!kime_core::context::is_from_input_method(u32::MAX));
}
