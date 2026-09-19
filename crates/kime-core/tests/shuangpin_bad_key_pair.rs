//! 双拼非法键对的候选保底：偶数长度输入含不可解码键对（自然码
//! xk = x+ao「xao」不存在）时，不能整串回退全拼——双拼键序含 v 等
//! 全拼不存在的字母，全拼切分必空 → 0 候选 → 候选窗消失、翻页失效。
//! 必须回退最长可解码偶数前缀，保住已敲部分的候选。

use kime_core::config::Config;
use kime_core::{Engine, Key};
use kime_shuangpin::Scheme;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn tmp_db(suffix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_spbp_{}_{}_{}.sqlite",
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

fn shuangpin_engine(db: &std::path::Path) -> Engine {
    let _ = kime_core::dict::Dict::open(db).expect("create schema");
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(
        "INSERT OR REPLACE INTO phrase(pinyin, text, freq, abbrev, user) VALUES
           ('bu''zhi','不知',5000,'bz',0),
           ('bu''zhi''dao','不知道',9999,'bzd',0);
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

fn candidates_of(e: &Engine) -> Vec<String> {
    e.candidates().iter().map(|c| c.text.clone()).collect()
}

#[test]
fn bad_key_pair_keeps_prefix_candidates() {
    let db = tmp_db("buvixk");
    let mut e = shuangpin_engine(&db);
    // buvi = bu+zhi 合法，尾部 xk = x+ao「xao」非法
    for c in "buvixk".chars() {
        e.key(k(c));
    }
    // preedit 必须是已解码前缀的读音，不能是原始字母（后者是整串回退全拼的签名）
    assert_eq!(e.preedit(), "buzhi", "preedit 应为前缀读音 buzhi");
    let cs = candidates_of(&e);
    assert!(!cs.is_empty(), "非法键对后候选不应为空");
    assert!(
        cs.contains(&"不知".to_string()),
        "最长可解码前缀 buvi 应保住「不知」, got {cs:?}"
    );
}

#[test]
fn bad_key_pair_after_three_chars_keeps_sentence() {
    // 用户报告的原场景：打了三个以上的字后候选消失
    let db = tmp_db("buvidkxk");
    let mut e = shuangpin_engine(&db);
    for c in "buvidkxk".chars() {
        e.key(k(c));
    }
    assert_eq!(e.preedit(), "buzhidao");
    let cs = candidates_of(&e);
    assert!(!cs.is_empty(), "三字后非法键对，候选不应为空");
    assert!(
        cs.contains(&"不知道".to_string()),
        "前缀 buvidk 应保住「不知道」, got {cs:?}"
    );
}

#[test]
fn good_key_pair_unchanged() {
    // 无非法键对的正常路径不能被前缀回退影响
    let db = tmp_db("buvidk");
    let mut e = shuangpin_engine(&db);
    for c in "buvidk".chars() {
        e.key(k(c));
    }
    assert_eq!(e.preedit(), "buzhidao");
    let cs = candidates_of(&e);
    assert!(cs.contains(&"不知道".to_string()));
}
