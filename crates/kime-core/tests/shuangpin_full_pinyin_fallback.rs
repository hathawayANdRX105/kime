//! 双拼模式下的全拼混输回退：ziranma 解码失败或解出错误音节时，
//! 必须回退全拼切分（与 fcitx5 双拼方案的全拼混输行为一致）。

use kime_core::config::Config;
use kime_core::{Engine, Key};
use kime_shuangpin::Scheme;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn tmp_db(suffix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_spfb_{}_{}_{}.sqlite",
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
    // Dict::open 先建 schema，再灌 fixture，最后重开给引擎用
    let _ = kime_core::dict::Dict::open(db).expect("create schema");
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(
        "INSERT OR REPLACE INTO phrase(pinyin, text, freq, abbrev, user) VALUES
           ('ni''hao','你好',5000,'nh',0),
           ('shi''jie','世界',9999,'sj',0),
           ('fan''gan','反感',600,'fg',0),
           ('fang''an','方案',900,'fa',0);
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
fn shuangpin_falls_back_to_full_pinyin_on_miss() {
    let db = tmp_db("fang");
    let mut e = shuangpin_engine(&db);
    // "fangan"：ziranma 解成 fa+neng（合法键对但查无词）→ 全拼回退，最优切分 fang+an
    for c in "fangan".chars() {
        e.key(k(c));
    }
    let cs = candidates_of(&e);
    assert!(
        cs.contains(&"方案".to_string()),
        "shuangpin miss must fall back to full pinyin, got {cs:?}"
    );
}

#[test]
fn shuangpin_accepts_full_pinyin_input() {
    let db = tmp_db("nihao");
    let mut e = shuangpin_engine(&db);
    // 全拼 nihao 在 ziranma 下解成 ni+ha（合法键对但查无词）→ 全拼回退命中「你好」
    for c in "nihao".chars() {
        e.key(k(c));
    }
    let cs = candidates_of(&e);
    assert!(
        cs.contains(&"你好".to_string()),
        "full pinyin under shuangpin should surface 你好, got {cs:?}"
    );
}
