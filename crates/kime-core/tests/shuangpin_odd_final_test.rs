//! 双拼奇数键的末键双意图验收测试。
//!
//! 钉住两件事：
//! 1. **末键作声母位**的单键/奇数键联想：按 `i`（自然码 i=ch 声母位）能出
//!    chi/chai 一类完整音节词，而不是零候选。
//! 2. **末键作韵母位**的单键/奇数键联想：按 `p`（自然码 p 作韵母位是
//!    un/ün 系列：小鹤 p 是 ie 系列）能出 lun/jun/chun/run 一类词。
//!
//! 背景：同一键在两套码表里身兼声母位与韵母位，而末键未成对时 kime 只按
//! `initial_of`（声母公共前缀）查——查拼音 "p" 零命中，用户看到的是「p 键
//! 完全没反应」。两条意图都要枚举成完整音节再查。

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::engine::{Engine, Key, Outcome};
use kime_shuangpin::Scheme;

fn tmp_db(suffix: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_odd_final_{}_{}_{}.sqlite",
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

/// 词条覆盖两套意图：
/// - 声母位：pai（p+韵母）、chi（ch+i）
/// - 韵母位：lun / jun / run（声母 + un 韵母）、xie / jie（声母 + ie 韵母）
const SEED: &str = "\
...
派\tpai\t900000
拍\tpai\t800000
吃\tchi\t950000
迟\tchi\t700000
论\tlun\t500000
军\tjun\t480000
润\trun\t300000
鞋\txie\t400000
姐\tjie\t380000
";

fn seeded_engine(suffix: &str, scheme: Scheme) -> (Engine, PathBuf, PathBuf) {
    let db = tmp_db(suffix);
    let yaml = std::env::temp_dir().join(format!(
        "kime_odd_final_seed_{}_{}.yaml",
        std::process::id(),
        suffix
    ));
    fs::write(&yaml, SEED).unwrap();
    let mut d = Dict::open(&db).unwrap();
    d.import(&yaml).unwrap();
    let e = Engine::new(
        d,
        Config {
            shuangpin: Some(scheme),
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

/// 自然码：末键 `p` 作韵母位是 un/ün 系列 → 单键 p 必须出 lun/jun/run 类词。
///
/// 修前：pending = initial_of('p') = "p"，查拼音 "p" 零命中 → 候选窗空，
/// 用户视角是「p 键打不出、按键完全没反应」。
#[test]
fn ziranma_single_final_key_lists_un_series_words() {
    let (mut e, db, yaml) = seeded_engine("ziranma_p", Scheme::Ziranma);
    let cands = type_keys(&mut e, "p");
    assert!(
        !cands.is_empty(),
        "单键 p（自然码的 un/ün 韵母位）必须有候选，实际: {cands:?}"
    );
    assert!(
        cands.contains(&"论".to_string()),
        "un 韵母位的「论」(lun) 应在列，实际: {cands:?}"
    );
    assert!(
        cands.contains(&"军".to_string()),
        "un 韵母位的「军」(jun) 应在列，实际: {cands:?}"
    );
    cleanup(&db, &yaml);
}

/// 小鹤：末键 `p` 作韵母位是 ie 系列 → 单键 p 必须出 xie/jie 类词。
#[test]
fn xiaohe_single_final_key_lists_ie_series_words() {
    let (mut e, db, yaml) = seeded_engine("xiaohe_p", Scheme::Xiaohe);
    let cands = type_keys(&mut e, "p");
    assert!(
        !cands.is_empty(),
        "单键 p（小鹤的 ie 韵母位）必须有候选，实际: {cands:?}"
    );
    assert!(
        cands.contains(&"鞋".to_string()),
        "ie 韵母位的「鞋」(xie) 应在列，实际: {cands:?}"
    );
    assert!(
        cands.contains(&"姐".to_string()),
        "ie 韵母位的「姐」(jie) 应在列，实际: {cands:?}"
    );
    cleanup(&db, &yaml);
}

/// 末键作**声母位**的意图同样要保留：自然码 `i` = ch 声母位 → chi 系词在列。
///
/// 这条覆盖的是修前那条死代码分支（`initial_of(last).is_empty()` 从不触发，
/// 声母位音节从未进过候选池）。
#[test]
fn ziranma_single_initial_key_lists_complete_syllable_words() {
    let (mut e, db, yaml) = seeded_engine("ziranma_i", Scheme::Ziranma);
    let cands = type_keys(&mut e, "i");
    assert!(
        !cands.is_empty(),
        "单键 i（自然码的 ch 声母位）必须有候选，实际: {cands:?}"
    );
    assert!(
        cands.contains(&"吃".to_string()),
        "ch 声母位的「吃」(chi) 应在列，实际: {cands:?}"
    );
    cleanup(&db, &yaml);
}

/// 偶数键（完整键对）路径不受影响：完整键对照常按音节查词。
#[test]
fn even_length_decode_is_unaffected() {
    let (mut e, db, yaml) = seeded_engine("even", Scheme::Ziranma);
    // pai = p + l（自然码）→ 完整键对
    let cands = type_keys(&mut e, "pl");
    assert_eq!(
        cands.first().map(String::as_str),
        Some("派"),
        "完整键对 pl 应直接命中 pai 词，实际: {cands:?}"
    );
    cleanup(&db, &yaml);
}
