//! 双拼整句联想：词库里没有整串词条时，Viterbi 组词是唯一的候选来源。
//! 修复前双拼分支从不调用整句 Viterbi，长句一律 0 候选、preedit 退回原始键串。

use kime_core::config::Config;
use kime_core::{Engine, Key};
use kime_shuangpin::Scheme;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn tmp_db(suffix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_spsent_{}_{}_{}.sqlite",
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

/// 词库只有单词，**没有**任何整句词条：整句只能靠 Viterbi 拼出来。
fn shuangpin_engine(db: &std::path::Path) -> Engine {
    let _ = kime_core::dict::Dict::open(db).expect("create schema");
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(
        "INSERT OR REPLACE INTO phrase(pinyin, text, freq, abbrev, user) VALUES
           ('ni''hao','你好',5000,'nh',0),
           ('shi''jie','世界',9000,'sj',0),
           ('ni','你',8000,'n',0),
           ('hao','好',8000,'h',0),
           ('shi','是',7000,'s',0),
           ('jie','界',3000,'j',0);
        ",
    )
    .unwrap();
    drop(conn);
    let dict = kime_core::dict::Dict::open(db).expect("reopen dict");
    let config = Config {
        dict_path: db.to_string_lossy().to_string(),
        shuangpin: Some(Scheme::Ziranma),
        ..Config::default()
    };
    Engine::new(dict, config)
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

fn type_keys(e: &mut Engine, keys: &str) {
    for c in keys.chars() {
        e.key(k(c));
    }
}

#[test]
fn shuangpin_sentence_composes_when_no_whole_phrase_exists() {
    let db = tmp_db("compose");
    let mut e = shuangpin_engine(&db);
    // nihkuijx = ni+hao+shi+jie（自然码）。词库无「你好世界」词条，
    // 只有「你好」+「世界」，整句必须由 Viterbi 合出来并置顶。
    type_keys(&mut e, "nihkuijx");

    let cs: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
    assert_eq!(
        cs.first().map(String::as_str),
        Some("你好世界"),
        "Viterbi 整句应作为首候选，实际 {cs:?}"
    );
    // 音节解码正确 → preedit 必须是拼音而非原始键串
    assert_eq!(e.preedit(), "nihaoshijie", "preedit 应显示解码后的拼音");
}

#[test]
fn shuangpin_sentence_survives_full_pinyin_retry() {
    let db = tmp_db("retry");
    let mut e = shuangpin_engine(&db);
    // 双拼路径解出的音节合法但整串查无词，会先走全拼重试；全拼重试同样无果时，
    // 双拼的解读才是对的 —— preedit/last_reading 必须回到双拼解码结果，
    // 不能被全拼重试留下的原始键串覆盖。
    type_keys(&mut e, "nihkuijx");
    assert_eq!(e.preedit(), "nihaoshijie");
    assert!(!e.candidates().is_empty(), "整句候选不应为空");
}
