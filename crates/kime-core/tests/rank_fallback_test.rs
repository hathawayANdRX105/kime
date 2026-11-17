//! 缺陷 A/B 回归：整串无词条时候选退回最长前缀音节；跨语言候选按
//! **`(层, 语言, 分数)`** 排序——同层内中文整体先于英文（用户裁定「保精确分块」），
//! 校准分数只在同语言同层内决定次序。
//! fixture 刻意只放单词且第二音节无字——Viterbi 拼不出全覆盖句，正是缺陷 A 的
//! 触发条件（修复前列表为空）。preedit 语义单列断言钉住：回退只补候选，不改显示。

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::engine::{Engine, Key};
use std::fs;
use std::path::PathBuf;

// 频率刻意拉开：你 = 中文表内最高频，打印/大约 居中，英文 day 是英文表内最高频、
// dab 是长尾。输入 `da` 时两语言都落在补全层（中文 `da'yin` / 英文 `day` 的前缀），
// 于是同层内中文恒先：打印 > 大约（分数）> day > dab（语言序把英文整段推到中文之后）。
// 这正是「提频后的英文词不得压过中文词」那条修复的回归。
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

/// 跨语言同层内**语言优先于分数**：输入 `da` 时候选顺序是「中文补全 → 英文补全」。
///
/// 这条断言钉住用户裁定「保精确分块」：两种语言的有效频率来自不同统计空间
/// （中文 10⁵~10⁷、英文词表 10³ 量级），归一化只保证本语言内部可比。若跨语言
/// 比校准分数，英文 day 作为词表最高频会顶到中文 打印 之前——用户实测 `ui` 的
/// 首候选因此变成 `UI` 而不是「是」。同层内中文恒先即可消除这类误比。
#[test]
fn cross_language_candidates_put_chinese_before_english_in_same_layer() {
    let (mut e, dir) = engine_at("cross_full", None);
    type_letters(&mut e, "da");
    let texts: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
    let pos = |t: &str| {
        texts
            .iter()
            .position(|x| x.eq_ignore_ascii_case(t))
            .unwrap_or_else(|| panic!("候选里缺少 {t}：{texts:?}"))
    };
    let (day, da_yin, da_yue, dab) = (pos("day"), pos("打印"), pos("大约"), pos("dab"));
    assert!(
        da_yin < day,
        "同层内中文恒先：打印应排在英文 day 之前（哪怕 day 是英文表最高频）：{texts:?}"
    );
    assert!(
        da_yue < day,
        "同层内中文恒先：大约应排在英文 day 之前：{texts:?}"
    );
    assert!(
        da_yin < da_yue,
        "同语言同层内才比分数：打印(500k) 在 大约(400k) 之前：{texts:?}"
    );
    assert!(
        day < dab,
        "同语言同层内才比分数：day(500) 在 dab(1) 之前：{texts:?}"
    );
    fs::remove_dir_all(dir).unwrap();
}

/// 双拼模式下同一结论：跨语言顺序只由语言序决定，与解码方式无关。
#[test]
fn shuangpin_cross_language_candidates_put_chinese_before_english() {
    let (mut e, dir) = engine_at("cross_sp", Some(kime_shuangpin::Scheme::Ziranma));
    type_letters(&mut e, "da");
    let texts: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
    let pos = |t: &str| {
        texts
            .iter()
            .position(|x| x.eq_ignore_ascii_case(t))
            .unwrap_or_else(|| panic!("候选里缺少 {t}：{texts:?}"))
    };
    let (day, da_yin, da_yue, dab) = (pos("day"), pos("打印"), pos("大约"), pos("dab"));
    assert!(
        da_yin < day,
        "双拼下同层内中文仍恒先：打印应在 day 之前：{texts:?}"
    );
    assert!(
        da_yue < day,
        "双拼下同层内中文仍恒先：大约应在 day 之前：{texts:?}"
    );
    assert!(
        da_yin < da_yue,
        "双拼下同语言同层内按分数：打印 在 大约 之前：{texts:?}"
    );
    assert!(
        day < dab,
        "双拼下同语言同层内按分数：day 在 dab 之前：{texts:?}"
    );
    fs::remove_dir_all(dir).unwrap();
}
