//! 整句候选不得压过直接命中读音的真词。
//!
//! Viterbi 的代价是 `ln(freq)` 可加的，所以「两个超高频单字」的组合永远比「一个真词」
//! 便宜。词库单字频率修好前，这个缺陷被 freq=0 掩盖；修好后直接表现为
//! 打 `ufme`(什么) 首候选变成「神么」、`jintian`(今天) 变成「级虐差」。
//! 整句是兜底手段，不是排名选手。

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::engine::{Engine, Key};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn tmp_db(suffix: &str, rows: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_srank_{}_{}_{}.sqlite",
        suffix,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_file(&path);
    let _ = Dict::open(&path).expect("create schema");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(rows).unwrap();
    drop(conn);
    path
}

fn engine(db: &std::path::Path) -> Engine {
    Engine::new(
        Dict::open(db).expect("open dict"),
        Config {
            dict_path: db.to_string_lossy().to_string(),
            ..Config::default()
        },
    )
}

fn keys(e: &mut Engine, s: &str) {
    for c in s.chars() {
        e.key(Key {
            ch: Some(c),
            code: 0,
            shift: false,
            ctrl: false,
            alt: false,
        });
    }
}

fn texts(e: &Engine) -> Vec<String> {
    e.candidates().iter().map(|c| c.text.clone()).collect()
}

#[test]
fn real_word_beats_the_composed_sentence() {
    // 「神/么/什」都是超高频单字，「什么」只是五十一万频的真词：
    // 纯按 ln(freq) 相加，神+么 必然更便宜，整句就会顶到第一。
    let db = tmp_db(
        "word",
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('shen','神',90000000,'s',0),
           ('shen','什',70000000,'s',0),
           ('me','么',80000000,'m',0),
           ('shen''me','什么',510349,'sm',0);",
    );
    let mut e = engine(&db);
    keys(&mut e, "shenme");
    let got = texts(&e);
    assert_eq!(
        got.first().map(String::as_str),
        Some("什么"),
        "直接命中读音的真词必须排第一，不能被整句组合挤掉，实际 {got:?}"
    );
    assert!(
        !got.contains(&"神么".to_string()) || got.first().unwrap() == "什么",
        "整句可以出现在列表里，但不能压过真词，实际 {got:?}"
    );
}

#[test]
fn sentence_leads_when_no_word_matches_the_reading() {
    // 只有单字、没有整串词条：这时整句是唯一来源，必须排第一（原需求不能改坏）
    let db = tmp_db(
        "alone",
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('shen','甚',90000000,'s',0),
           ('me','么',80000000,'m',0);",
    );
    let mut e = engine(&db);
    keys(&mut e, "shenme");
    let got = texts(&e);
    assert_eq!(
        got.first().map(String::as_str),
        Some("甚么"),
        "无整串词条时整句组合应是首候选，实际 {got:?}"
    );
}
