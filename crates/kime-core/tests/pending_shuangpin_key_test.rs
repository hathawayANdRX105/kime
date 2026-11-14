//! 半截键（只敲了双拼键对的前一半）必须按**声母**去查，不是按那个字母本身。
//!
//! 自然码里 `u` 是 sh 的键位。修前 engine 直接把 `"u"` 当拼音前缀丢给
//! `lookup_prefix`，于是查的是「拼音以 u 开头的词」，半截状态出的候选与用户要打的
//! 字毫无关系（实测 `preedit=u` 时零候选）。修后 `u` 展开成 `sh`，preedit 也显示 `sh`。

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::engine::{Engine, Key, Outcome};
use kime_shuangpin::Scheme;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn tmp_db(suffix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_pending_{}_{}_{}.sqlite",
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

/// `是`=shi(经 u 键)、`乌`=wu（拼音真的以 u 开头，用来抓「按字母查」的错路）。
fn engine(db: &std::path::Path) -> Engine {
    let _ = Dict::open(db).expect("create schema");
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(
        "INSERT OR REPLACE INTO phrase(pinyin, text, freq, abbrev, user) VALUES
           ('shi','是',9000,'s',0),
           ('shi','事',8000,'s',0),
           ('wu','乌',7000,'w',0),
           ('u','优',6000,'u',0);",
    )
    .unwrap();
    drop(conn);
    let dict = Dict::open(db).expect("open dict");
    Engine::new(
        dict,
        Config {
            dict_path: db.to_string_lossy().to_string(),
            shuangpin: Some(Scheme::Ziranma),
            ..Config::default()
        },
    )
}

fn type_str(e: &mut Engine, keys: &str) {
    for c in keys.chars() {
        e.key(Key {
            ch: Some(c),
            code: 0,
            shift: false,
            ctrl: false,
            alt: false,
        });
    }
}

#[test]
fn pending_shuangpin_key_queries_its_initial() {
    let db = tmp_db("initial");
    let mut e = engine(&db);
    type_str(&mut e, "u");
    let got: Vec<&str> = e.candidates().iter().map(|c| c.text.as_str()).collect();
    assert!(
        got.contains(&"是") && got.contains(&"事"),
        "半截键 u 应给出 sh 系的字，实际 {got:?}"
    );
    // 末键 u 还有第二种意图：它作**韵母位**（自然码 wu = w+u，声母键还没按）。
    // 这类词现在也会进候选（「乌」拼音是完整音节 wu，与「按字母 u 查」不是同一条
    // 路），但 u 是 sh 声母键，用户按 u 想要的是 sh 系——sh 系必须排在前面。
    // （「优」的拼音是裸字母 u，不是合法音节，任何路径都不该带出它。）
    let shi_pos = got.iter().position(|t| *t == "是");
    let wu_pos = got.iter().position(|t| *t == "乌");
    assert!(
        !got.contains(&"优"),
        "拼音是裸字母 u 的词不该进候选，实际 {got:?}"
    );
    if let (Some(shi), Some(wu)) = (shi_pos, wu_pos) {
        assert!(shi < wu, "sh 系词应排在韵母位意图的词之前，实际 {got:?}");
    }
    // preedit 要显示用户正在拼的声母，而不是裸键
    assert_eq!(e.preedit(), "sh", "preedit 应展开成 sh");
}

#[test]
fn pending_key_completes_into_the_full_syllable() {
    let db = tmp_db("complete");
    let mut e = engine(&db);
    type_str(&mut e, "ui"); // u + i = shi
    assert_eq!(e.preedit(), "shi");
    let top = e.candidates().first().map(|c| c.text.clone());
    assert_eq!(top.as_deref(), Some("是"), "补全后首候选应是「是」");
    let out = e.key(Key {
        ch: None,
        code: 57,
        shift: false,
        ctrl: false,
        alt: false,
    });
    assert_eq!(out, Outcome::Commit("是".into()), "选词立即上屏");
}
