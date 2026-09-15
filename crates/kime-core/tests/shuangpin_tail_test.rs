//! 双拼半截键末音节补全验收测试。
//!
//! 钉住：零声母半截键（y/w/元音，公共前缀塌缩为空）的补全语义——
//! 敲 `ke`+`y` 的意图是「末音节以 y 开头的完整音节」（ke'yi 可以上），这些词
//! 必须排在任意 ke* 词之前；单键简拼同理（`y` → yi 系词在前）。
//! 有公共前缀的半截键（u→sh 等）行为不变。

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::engine::{Engine, Key, Outcome};
use kime_shuangpin::Scheme;

fn tmp_db(suffix: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_sp_tail_{}_{}_{}.sqlite",
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

/// ziranma：ke = "ke"，yi = "yi"（零声母 y+韵母），ni = "ni"。
/// 「可以」用超高频，「可乐」是 ke'le 对照（le 不在 y 系音节里），
/// 「你」是 ni 系对照（y 单键简拼时不得压过 yi 系补全词）。
const SEED: &str = "\
...
可以\tke yi\t600000
可意\tke yi\t3000
可乐\tke le\t90000
可可\tke ke\t80000
科 \tke\t70000
以\tyi\t500000
一\tyi\t480000
呀\tya\t120000
你\tni\t700000
泥\tni\t300000
";

fn seeded_engine(suffix: &str) -> (Engine, PathBuf, PathBuf) {
    let db = tmp_db(suffix);
    let yaml = std::env::temp_dir().join(format!(
        "kime_sp_tail_seed_{}_{}.yaml",
        std::process::id(),
        suffix
    ));
    fs::write(&yaml, SEED).unwrap();
    let mut d = Dict::open(&db).unwrap();
    d.import(&yaml).unwrap();
    let e = Engine::new(
        d,
        Config {
            shuangpin: Some(Scheme::Ziranma),
            ..Config::default()
        },
    );
    (e, db, yaml)
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

fn type_keys(e: &mut Engine, letters: &str) -> Vec<String> {
    for c in letters.chars() {
        if let Outcome::Commit(text) = e.key(ch(c)) {
            return vec![format!("<commit:{text}>")];
        }
    }
    e.candidates().iter().map(|c| c.text.clone()).collect()
}

fn cleanup(db: &PathBuf, yaml: &PathBuf) {
    let _ = fs::remove_file(db);
    let _ = fs::remove_file(yaml);
}

#[test]
fn half_key_y_prioritizes_complete_yi_syllable_words() {
    let (mut e, db, yaml) = seeded_engine("key");
    // ke + 半截 y → ke'yi 补全词（可以/可意）在前，裸 ke* 词（可乐/可可）随后。
    let cands = type_keys(&mut e, "key");
    assert_eq!(
        cands.first().map(String::as_str),
        Some("可以"),
        "key 半截补全应以 ke'yi 高频词开头，实际: {cands:?}"
    );
    assert!(
        cands.contains(&"可意".to_string()),
        "同音节的冷门 ke'yi 词也应在列，实际: {cands:?}"
    );
    cleanup(&db, &yaml);
}

#[test]
fn single_zero_initial_key_lists_its_syllable_words_first() {
    let (mut e, db, yaml) = seeded_engine("solo");
    // 单键 y（半截）：yi 系补全词在前，其它音节词（你/泥）不得插队。
    let cands = type_keys(&mut e, "y");
    assert_eq!(
        cands.first().map(String::as_str),
        Some("以"),
        "y 简拼补全应以 yi 高频词开头，实际: {cands:?}"
    );
    cleanup(&db, &yaml);
}

#[test]
fn even_length_decode_is_unchanged() {
    let (mut e, db, yaml) = seeded_engine("even");
    // 完整键对路径不经过补全：ke+ke → 可可 按频正常排。
    let cands = type_keys(&mut e, "keke");
    assert_eq!(
        cands.first().map(String::as_str),
        Some("可可"),
        "完整键对行为不得改变，实际: {cands:?}"
    );
    cleanup(&db, &yaml);
}
