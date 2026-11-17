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

/// 带指定词条的引擎：`rows` 是 `(拼音, 词, 频率)`。
///
/// 默认 `engine()` 的 fixture 只有 shi/wu/u，测不了 a 系与 un 系。
fn engine_with<S: AsRef<str>>(db: &std::path::Path, rows: &[(S, S, i64)]) -> Engine {
    let _ = Dict::open(db).expect("create schema");
    let conn = rusqlite::Connection::open(db).unwrap();
    for (pinyin, text, freq) in rows {
        let pinyin = pinyin.as_ref();
        let text = text.as_ref();
        conn.execute(
            "INSERT OR REPLACE INTO phrase(pinyin, text, freq, abbrev, user) VALUES (?1, ?2, ?3, ?4, 0)",
            rusqlite::params![
                pinyin,
                text,
                freq,
                text.chars().next().unwrap_or(' ').to_string()
            ],
        )
        .unwrap();
    }
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

/// 孤立元音键：零声母 `a`（Ziranma 编码 `aa`）单独敲时**只走声母位**，
/// 不得把 `a` 当韵母位展开出 ba/da 这类**带声母**的音节。
///
/// 回归的 bug：用户敲 `a` 首屏是「把/吧」，而 `吧` 是 b 开头的字。
#[test]
fn lone_vowel_key_does_not_expand_final_intent() {
    let db = tmp_db("vowel");
    let mut e = engine_with(
        &db,
        &[
            ("a", "啊", 9000),
            ("ba", "吧", 8000),
            ("da", "大", 7000),
            ("ma", "吗", 6000),
        ],
    );
    type_str(&mut e, "a");
    let got: Vec<&str> = e.candidates().iter().map(|c| c.text.as_str()).collect();
    assert!(!got.is_empty(), "单敲 a 必须有候选，实际 {got:?}");
    assert_eq!(got, vec!["啊"], "单敲 a 只应出 a 系，实际 {got:?}");
    // preedit 展示原始键
    assert_eq!(e.preedit(), "a", "preedit 应显示原始键 a");
}

/// 双拼 `aa` 仍是「啊」的编码：孤立元音键的韵母位意图没有被误删。
#[test]
fn double_a_completes_into_a() {
    let db = tmp_db("vowel2");
    let mut e = engine_with(&db, &[("a", "啊", 9000), ("ba", "吧", 8000)]);
    type_str(&mut e, "aa");
    assert_eq!(e.preedit(), "a", "aa 解码为 a");
    let top = e.candidates().first().map(|c| c.text.clone());
    assert_eq!(top.as_deref(), Some("啊"), "aa 首候选应是「啊」");
}

/// 孤立辅音键 `p` 的韵母位意图必须保留：单敲 `p` 要出 un/ün 系列
/// （lun/jun/run…），这是「韵母位意图」的设计契约。
#[test]
fn lone_consonant_key_keeps_final_intent() {
    let db = tmp_db("vowel3");
    let mut e = engine_with(
        &db,
        &[
            ("lun", "论", 9000),
            ("jun", "军", 8000),
            ("cun", "村", 7000),
        ],
    );
    type_str(&mut e, "p");
    let got: Vec<&str> = e.candidates().iter().map(|c| c.text.as_str()).collect();
    assert!(
        got.contains(&"论") && got.contains(&"军"),
        "单敲 p（Ziranma 韵母位 un/ün）应出 lun/jun 系列，实际 {got:?}"
    );
}

/// 两个意图的**相对次序**：声母位意图整体在韵母位意图之前。
///
/// 回归的契约矛盾：`merge_half_key_intents` 旧实现把两批压平后按分数统一排，
/// 从未保证过「声母位优先」——`pending_shuangpin_key_queries_its_initial` 里
/// `是 < 乌` 只是因为 fixture 中 是(9000) 恰比 乌(7000) 频高。
///
/// 这里把两个意图的频率**反过来**（韵母位的 `乌` 更高），顺序仍须由意图决定：
/// 声母位意图的词全部排在韵母位意图之前。
#[test]
fn initial_intent_sorts_before_final_intent_regardless_of_score() {
    let db = tmp_db("intent_order");
    let mut e = engine_with(
        &db,
        &[
            ("shi", "事", 1_000),    // 声母位：sh 系（低频率）
            ("wu", "乌", 9_000_000), // 韵母位：*u 系（高频率）
            ("bu", "不", 8_000_000),
        ],
    );
    type_str(&mut e, "u");
    let got: Vec<&str> = e.candidates().iter().map(|c| c.text.as_str()).collect();
    let shi = got
        .iter()
        .position(|t| *t == "事")
        .expect("声母位词「事」应在列");
    let wu = got
        .iter()
        .position(|t| *t == "乌")
        .expect("韵母位词「乌」应在列");
    assert!(
        shi < wu,
        "声母位意图必须整体在韵母位之前，即使韵母位词频高得多：{got:?}"
    );
}

/// 韵母位意图的**保底份额**：声母位词很多时，韵母位也不能被挤出候选窗。
///
/// 回归：旧实现里声母位查询自己就按 `candidate_limit` 截断，合批后整体再截断，
/// 于是声母位读音一多韵母位就 0 条可见——真实词库实测敲 `p` 首 50 条里一条
/// un/ün 系列都没有，韵母位意图等于不存在。
#[test]
fn final_intent_keeps_a_reserved_quota() {
    // 声母位：30 个 p 系音节（pa/pai/pan/pang/pao/pei/pen/peng/pi/pian/…）
    let finals = [
        "pa", "pai", "pan", "pang", "pao", "pei", "pen", "peng", "pi", "pian", "piao", "pie",
        "pin", "ping", "po", "pou", "pu", "pa", "pai", "pan", "pang", "pao", "pei", "pen", "peng",
        "pi", "pian", "piao", "pie", "pin",
    ];
    let mut rows: Vec<(String, String, i64)> = finals
        .iter()
        .enumerate()
        .map(|(i, f)| (f.to_string(), format!("声母{i}"), 1_000_000 + i as i64))
        .collect();
    // 韵母位：un/ün 系列
    rows.push(("lun".to_string(), "论".to_string(), 10));
    rows.push(("jun".to_string(), "军".to_string(), 9));

    let db = tmp_db("quota");
    let mut e = engine_with(&db, &rows);
    type_str(&mut e, "p");
    let got: Vec<&str> = e.candidates().iter().map(|c| c.text.as_str()).collect();
    assert!(
        got.contains(&"论") && got.contains(&"军"),
        "声母位占满窗口时，韵母位必须有保底份额（论/军 应可见）：{got:?}"
    );
    // 且声母位仍在前
    let first_final = got.iter().position(|t| *t == "论").expect("论应在列");
    assert!(
        first_final > 0,
        "声母位意图应排在韵母位之前，论不该是首候选：{got:?}"
    );
}
