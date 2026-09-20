//! Esc 键契约：有组合时清空并消费；空组合时放行给应用。
//!
//! 修复前 Esc 无条件 Consumed，中文模式光标空着按 Esc 被吞掉——退出全屏
//! vim、关闭对话框全失效，与英文模式行为不一致。空组合放行与 Backspace
//! 的同族处理对齐。

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::engine::{Engine, Key, Outcome};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const KEY_ESC: u32 = 1;

fn tmp_db(suffix: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_esc_{}_{}_{}.sqlite",
        suffix,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_file(&path);
    path
}

fn engine_with_fixture() -> (Engine, PathBuf) {
    let db = tmp_db("esc");
    let _ = Dict::open(&db).expect("open dict");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "INSERT OR REPLACE INTO phrase(pinyin, text, freq, abbrev, user) VALUES
           ('ni''hao','你好',5000,'nh',0),
           ('ni''hou','泥猴', 100,'nh',0),
           ('shi''jie','世界',9999,'sj',0),
           ('a',     '安',  8000,'a', 0);",
    )
    .unwrap();
    drop(conn);
    let dict = Dict::open(&db).expect("reopen dict for lookups");
    let engine = Engine::new(dict, Config::default());
    (engine, db)
}

fn k(ch: char) -> Key {
    Key {
        ch: Some(ch),
        code: 0,
        shift: false,
        ctrl: false,
        alt: false,
    }
}

fn code_k(code: u32) -> Key {
    Key {
        ch: None,
        code,
        shift: false,
        ctrl: false,
        alt: false,
    }
}

#[test]
fn esc_clears_composition() {
    let (mut e, db) = engine_with_fixture();
    for c in "nih".chars() {
        e.key(k(c));
    }
    assert!(!e.preedit().is_empty());
    assert_eq!(e.key(code_k(KEY_ESC)), Outcome::Consumed);
    assert!(e.preedit().is_empty());
    assert!(e.candidates().is_empty());
    let _ = fs::remove_file(&db);
}

#[test]
fn esc_on_empty_composition_is_ignored() {
    // 空组合按 Esc 必须放行：退出全屏 vim、关对话框都靠它。
    // 之前无条件 Consumed 吞掉，与英文模式不一致。
    let (mut e, db) = engine_with_fixture();
    assert_eq!(e.key(code_k(KEY_ESC)), Outcome::Ignored);
    assert!(e.chinese(), "不得切换模式");
    // 有组合时仍正常清空
    for c in "ni".chars() {
        e.key(k(c));
    }
    assert_eq!(e.key(code_k(KEY_ESC)), Outcome::Consumed);
    assert!(e.preedit().is_empty());
    let _ = fs::remove_file(&db);
}
