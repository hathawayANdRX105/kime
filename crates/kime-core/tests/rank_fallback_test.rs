//! 缺陷 A/B 回归：整串无词条时候选退回最长前缀音节；英文精确词高于中文补全。
//!
//! fixture 刻意只放单词且第二音节无字——Viterbi 拼不出全覆盖句，正是缺陷 A 的
//! 触发条件（修复前列表为空）。preedit 语义单列断言钉住：回退只补候选，不改显示。

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::engine::{Engine, Key};
use std::fs;
use std::path::PathBuf;

const CN: &str = "...\n你\tni\n泥\tni\n打印\tda yin\n大约\tda yue\n";
const EN: &str = "...\nday\tday\t500\nhelp\thelp\t400\n";

fn tmp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "kime_rankfb_{tag}_{}_{}",
        std::process::id(),
        nanos
    ))
}

/// 只放单词（你/泥/打印/大约），**不放** ni'qia / ni'qia* 任何词条——
/// 第二音节 qia 无字，Viterbi 无全覆盖句。
fn engine_at(tag: &str, scheme: Option<kime_shuangpin::Scheme>) -> (Engine, PathBuf) {
    let dir = tmp_dir(tag);
    fs::create_dir_all(&dir).unwrap();
    let yaml = dir.join("cn.dict.yaml");
    fs::write(&yaml, CN).unwrap();
    let en_yaml = dir.join("en.dict.yaml");
    fs::write(&en_yaml, EN).unwrap();
    let mut dict = Dict::open(dir.join("dict.sqlite3")).unwrap();
    dict.import(&yaml).unwrap();
    dict.import_english(&en_yaml).unwrap();
    let e = Engine::new(
        dict,
        Config {
            shuangpin: scheme,
            ..Config::default()
        },
    );
    (e, dir)
}

fn type_letters(e: &mut Engine, s: &str) {
    for c in s.chars() {
        let _ = e.key(Key {
            ch: Some(c),
            code: 0,
            shift: false,
            ctrl: false,
            alt: false,
        });
    }
}

#[test]
fn full_pinyin_falls_back_to_longest_prefix_syllables() {
    let (mut e, dir) = engine_at("full", None);
    type_letters(&mut e, "niqia");
    let cands = e.candidates();
    assert!(
        !cands.is_empty(),
        "整串无词条时候选必须回退到首音节，不能空：preedit={}",
        e.preedit()
    );
    assert_eq!(e.preedit(), "niqia", "回退不得改动 preedit 显示");
    assert_eq!(cands[0].pinyin, "ni", "首候选读音必须是首音节 ni");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn shuangpin_falls_back_to_longest_prefix_syllables() {
    // 自然码 niqw = ni + qw(ia?)……取一个双拼解出 [ni, x] 的键串：
    // 自然码 q→iu? 直接用「 niq 」奇数位=半截键，解出 [ni] + 声母 q。
    let (mut e, dir) = engine_at("sp", Some(kime_shuangpin::Scheme::Ziranma));
    type_letters(&mut e, "niq");
    let cands = e.candidates();
    assert!(
        !cands.is_empty(),
        "双拼半截+无词条时候选必须非空：preedit={}",
        e.preedit()
    );
    assert_eq!(e.preedit(), "niq", "回退不得改动 preedit（保持原始键串）");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn single_syllable_input_needs_no_fallback() {
    let (mut e, dir) = engine_at("single", None);
    type_letters(&mut e, "ni");
    assert!(!e.candidates().is_empty(), "单音节直查本就非空");
    assert_eq!(e.preedit(), "ni");
    fs::remove_dir_all(dir).unwrap();
}

/// 全拼中英文精确命中优先于不匹配拼音的中文补全。
#[test]
fn full_pinyin_english_exact_precedes_chinese_completion() {
    let (mut e, dir) = engine_at("en_full", None);
    type_letters(&mut e, "day");
    let texts: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
    let en_pos = texts
        .iter()
        .position(|t| t.eq_ignore_ascii_case("day"))
        .expect("英文精确词 day 在场");
    let chinese_pos = texts
        .iter()
        .position(|t| t == "打印")
        .expect("中文补全 打印 在场");
    assert!(en_pos < chinese_pos, "英文精确词应先于中文补全：{texts:?}");
    fs::remove_dir_all(dir).unwrap();
}

/// 双拼模式同样按原始按键串匹配英文精确词，且优先于中文补全。
#[test]
fn shuangpin_english_exact_precedes_chinese_completion() {
    let (mut e, dir) = engine_at("en_sp", Some(kime_shuangpin::Scheme::Ziranma));
    type_letters(&mut e, "day");
    let texts: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
    let en_pos = texts
        .iter()
        .position(|t| t.eq_ignore_ascii_case("day"))
        .expect("英文精确词 day 在场");
    let chinese_pos = texts
        .iter()
        .position(|t| t == "打印" || t == "大约")
        .expect("中文补全在场");
    assert!(
        en_pos < chinese_pos,
        "双拼英文精确词应先于中文补全：{texts:?}"
    );
    fs::remove_dir_all(dir).unwrap();
}
