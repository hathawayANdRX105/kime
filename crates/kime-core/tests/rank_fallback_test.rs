//! 缺陷 A/B 回归：整串无词条时候选退回最长前缀音节；跨语言候选按**分数**交错
//! （不再有「中文整块在前」或「英文整块在前」的语言分块）。
//! fixture 刻意只放单词且第二音节无字——Viterbi 拼不出全覆盖句，正是缺陷 A 的
//! 触发条件（修复前列表为空）。preedit 语义单列断言钉住：回退只补候选，不改显示。

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::engine::{Engine, Key};
use std::fs;
use std::path::PathBuf;

// 频率刻意拉开：你 = 中文表内最高频，打印/大约 居中，英文 day 是英文表内最高频、
// dab 是长尾。跨语言只比校准分数，因此这四条能同时证明「英文能插到中文前面」
// 与「中文也能压过英文长尾」。
const CN: &str =
    "...\n你\tni\t9000000\n泥\tni\t8000000\n打印\tda yin\t500000\n大约\tda yue\t400000\n";
const EN: &str = "...\nday\tday\t500\ndab\tdab\t1\nhelp\thelp\t400\n";

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

/// 跨语言按分数交错：英文 day（英文表内最高频）排在中文 打印 之前，
/// 中文 打印 又排在英文长尾 dab 之前。
///
/// 这条断言同时否掉两种语言分块：若「中文整块在前」则 day 会在 打印 之后；
/// 若「英文整块在前」则 dab 会在 打印 之前。
#[test]
fn cross_language_candidates_order_by_score() {
    let (mut e, dir) = engine_at("cross_full", None);
    type_letters(&mut e, "da");
    let texts: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
    let pos = |t: &str| {
        texts
            .iter()
            .position(|x| x.eq_ignore_ascii_case(t))
            .unwrap_or_else(|| panic!("候选里缺少 {t}：{texts:?}"))
    };
    let (day, da_yin, dab) = (pos("day"), pos("打印"), pos("dab"));
    assert!(day < da_yin, "英文高分词应排在中文中频词之前：{texts:?}");
    assert!(da_yin < dab, "中文中频词应排在英文长尾词之前：{texts:?}");
    fs::remove_dir_all(dir).unwrap();
}

/// 双拼模式下同一结论：跨语言顺序只看分数，与解码方式无关。
#[test]
fn shuangpin_cross_language_candidates_order_by_score() {
    let (mut e, dir) = engine_at("cross_sp", Some(kime_shuangpin::Scheme::Ziranma));
    type_letters(&mut e, "da");
    let texts: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
    let pos = |t: &str| {
        texts
            .iter()
            .position(|x| x.eq_ignore_ascii_case(t))
            .unwrap_or_else(|| panic!("候选里缺少 {t}：{texts:?}"))
    };
    let (day, da_yin, dab) = (pos("day"), pos("打印"), pos("dab"));
    assert!(
        day < da_yin,
        "双拼下英文高分词仍应排在中文中频词之前：{texts:?}"
    );
    assert!(
        da_yin < dab,
        "双拼下中文中频词仍应排在英文长尾词之前：{texts:?}"
    );
    fs::remove_dir_all(dir).unwrap();
}
