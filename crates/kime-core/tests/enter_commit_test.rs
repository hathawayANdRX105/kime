//! 工单第 3 条：Enter（code 28）不再「看起来切换中英文」。
//! 新契约：有候选 → 提交高亮候选并 learn；无候选 → 原样上屏字母。
//! 两条分支都绝不改 `chinese` 模式（切模式只属于 Shift）。

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
const KEY_EQUAL: u32 = 13;

fn type_str(e: &mut Engine, s: &str) {
    for c in s.chars() {
        e.key(ch(c));
    }
}

#[test]
fn enter_with_candidates_commits_highlight_and_keeps_chinese() {
    let db = tmp_db("top");
    let mut e = engine(
        &db,
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('ni''hao','你好',5000,'nh',0);",
    );
    type_str(&mut e, "nihao");
    assert_eq!(e.preedit(), "nihao");
    let out = e.key(code(KEY_ENTER));
    assert_eq!(
        out,
        Outcome::Commit("你好".into()),
        "Enter 必须上屏选中候选而非字母"
    );
    assert!(e.chinese(), "Enter 不得切换中英文模式");
    assert!(e.preedit().is_empty(), "组合必须清空");
    assert!(e.candidates().is_empty(), "候选必须清空");
    // learn 确实发生：提交走的是与空格选词相同的用户词回写（重开库看 user 行）。
    drop(e);
    let d2 = Dict::open(&db).unwrap();
    let top = d2.top_user(10).unwrap();
    assert!(
        top.iter().any(|c| c.text == "你好"),
        "Enter 提交后应有用户词记录，实际 {top:?}"
    );
    let _ = fs::remove_file(&db);
}

#[test]
fn enter_commits_the_page_highlight_not_the_global_top() {
    // 12 个同读音候选，翻到第 2 页按 Enter → 上屏第 11 个（页首 = 高亮项）。
    let db = tmp_db("page");
    let mut rows =
        String::from("INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES");
    for i in 0..12 {
        rows.push_str(&format!(
            "('ni','词{i}',{},'n',0){}\n",
            100 - i,
            if i == 11 { ";" } else { "," }
        ));
    }
    let mut e = engine(&db, &rows);
    type_str(&mut e, "ni");
    e.key(code(KEY_EQUAL));
    assert_eq!(e.page().0, 1, "应翻到第 2 页");
    let out = e.key(code(KEY_ENTER));
    assert_eq!(out, Outcome::Commit("词10".into()));
    assert!(e.chinese());
    let _ = fs::remove_file(&db);
}

#[test]
fn enter_without_candidates_commits_raw_letters_and_keeps_chinese() {
    let db = tmp_db("raw");
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
        "无候选时 Enter 原样上屏字母（英文/网址）"
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
