//! 工单第 3 条（用户定稿契约）：中文模式下按 Enter ＝「这串不是拼音」。
//! 有组合 → 原样上屏字母串（打英文/网址的习惯），中文模式保持不变、不 learn；
//! 无组合 → Ignored，回车照常放行给应用。选词是空格/数字的事，与 Enter 无关。

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::{Engine, Key, Outcome};

fn tmp_db(prefix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_enter_{}_{}_{}.sqlite",
        prefix,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_file(&path);
    path
}

fn engine(db: &std::path::Path, rows: &str) -> Engine {
    let _ = Dict::open(db).expect("create schema");
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(rows).unwrap();
    drop(conn);
    let dict = Dict::open(db).expect("reopen dict");
    Engine::new(
        dict,
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

fn code(code: u32) -> Key {
    Key {
        ch: None,
        code,
        shift: false,
        ctrl: false,
        alt: false,
    }
}

const KEY_ENTER: u32 = 28;

fn type_str(e: &mut Engine, s: &str) {
    for c in s.chars() {
        e.key(ch(c));
    }
}

#[test]
fn enter_commits_raw_letters_even_with_candidates() {
    // 用户诉求：Enter 永远上屏原始字母串、面板退出，不提交候选词——
    // 声明「我打的是字母不是拼音」。选词用空格/数字。
    let db = tmp_db("raw");
    let mut e = engine(
        &db,
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('ni''hao','你好',5000,'nh',0);",
    );
    type_str(&mut e, "nihao");
    assert!(
        e.candidates().iter().any(|c| c.text == "你好"),
        "种子词库下 nihao 应有候选"
    );
    let out = e.key(code(KEY_ENTER));
    match out {
        Outcome::Commit(text) => assert_eq!(text, "nihao", "Enter 上屏原串，不提交候选"),
        other => panic!("expected Commit(nihao), got {:?}", other),
    }
    assert!(e.chinese(), "Enter 绝不切换中英文模式");
    assert!(e.preedit().is_empty(), "组合必须清空");
    assert!(e.candidates().is_empty(), "面板必须退出");
    let _ = fs::remove_file(&db);
}

#[test]
fn enter_without_candidates_commits_raw_letters_and_keeps_chinese() {
    let db = tmp_db("nocands");
    let mut e = engine(
        &db,
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('ni''hao','你好',5000,'nh',0);",
    );
    type_str(&mut e, "zzz");
    assert!(e.candidates().is_empty(), "种子词库下 zzz 不应有候选");
    let out = e.key(code(KEY_ENTER));
    assert_eq!(
        out,
        Outcome::Commit("zzz".into()),
        "无候选时 Enter 原样上屏字母，不转发回车键"
    );
    assert!(e.chinese(), "Enter 上屏字母同样不得切换模式");
    assert!(e.preedit().is_empty());
    let _ = fs::remove_file(&db);
}

#[test]
fn enter_with_empty_state_is_ignored() {
    let db = tmp_db("empty");
    let mut e = engine(&db, "SELECT 1;");
    assert_eq!(e.key(code(KEY_ENTER)), Outcome::Ignored);
    assert!(e.chinese());
    let _ = fs::remove_file(&db);
}
