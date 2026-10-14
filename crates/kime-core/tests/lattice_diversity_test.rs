//! 词图候选多样性（Wave 2）验收：B1 每格多候选词、B2 束宽提到 8、B4 缺音节占位。
//!
//! 全部钉在 lattice 层：临时 sqlite + 直接 `INSERT INTO phrase`（`Dict::open`
//! 时按 phrase 表建索引与 total_freq），直接调 `viterbi_sentences[_seeded]`，
//! 断言可观察的整句候选集合（文本与顺序）。引擎侧的落位/排序
//! （place_sentences）不在此文件范围内。
//!
//! 本文件同时承接原 lattice.rs 内联测试（canon 约定：测试统一放 tests/）：
//! 原内联测试里两条「无词 → 空」的行为由 B4 占位节点 + 占位句门禁后
//! 变回空返回，按当前语义重钉。
//!
//! 行为变化（改前 → 改后）：
//! - 2+2 切分：束宽 3 时第四便宜的组合被剪掉；8 后完整现身；
//! - 同格同音词：只出最高频那一个；现在全部成边、全部现身；
//! - 缺音节：穿过该音节的所有路径断掉、整句落空；现在占位边兜底让路径
//!   算得出代价，但占位句 text 含拼音字母、不可上屏，故不进候选列
//!   （qingjian has_placeholder 同款门禁）；
//! - 束宽上限：文本互异的切分多于 8 条时仍按代价升序截到 8 条。

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::dict::{Candidate, Dict};
use kime_core::lattice::{
    viterbi_sentences, viterbi_sentences_seeded, Seed, SpanCache, MAX_SPAN_CACHE_ENTRIES,
};

/// 造一个只含给定 phrase 行的临时词库，返回重新 open 的 Dict（索引从 phrase 表重建）
/// 与 db 路径（测试方负责收尾 `fs::remove_file`）。
fn open_dict(suffix: &str, rows: &str) -> (Dict, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "kime_lattice_diversity_{}_{}_{}.sqlite",
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
    let dict = Dict::open(&path).expect("open dict");
    (dict, path)
}

fn reading(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn texts(cands: &[Candidate]) -> Vec<String> {
    cands.iter().map(|c| c.text.clone()).collect()
}

// ── B2：束宽提升，2+2 切分不再被剪 ────────────────────────────────────

#[test]
fn two_plus_two_split_survives_beam_pruning() {
    // 4 音节输入：词库里有 3 条比 2+2 更便宜的文本——整词「你好世界」（1 词）、
    // 1+3「你 + 好时节」、3+1「你好时 + 界」；2+2（「您好 + 世界」，您好是
    // 同音异文词）排第 4。旧行为（PATHS_PER_NODE=3）把它整条剪掉；束宽提到
    // 8 后必须完整现身（多候选词/占位对结果无影响：本 fixture 每个跨度恰一个词、
    // 所有音节都有词可查）。
    let (dict, db) = open_dict(
        "s22",
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('ni','你',400000,'n',0),
           ('jie','界',200000,'j',0),
           ('ni''hao''shi''jie','你好世界',500000,'nh',0),
           ('hao''shi''jie','好时节',300000,'hsj',0),
           ('ni''hao''shi','你好时',250000,'nhs',0),
           ('ni''hao','您好',3000,'nh',0),
           ('shi''jie','世界',2500,'sj',0);",
    );
    let got = viterbi_sentences(&dict, &reading(&["ni", "hao", "shi", "jie"]));
    assert_eq!(
        texts(&got),
        vec![
            "你好世界".to_string(),
            "你好时节".to_string(),
            "你好时界".to_string(),
            "您好世界".to_string(),
        ],
        "2+2 切分（您好+世界）必须现身且按代价升序：实际 {got:?}"
    );
    let _ = fs::remove_file(&db);
}

// ── B1：同格同音词全部成边 ────────────────────────────────────────────

#[test]
fn homophone_words_all_become_edges() {
    // shen 下有两个同音字（神 > 什），整串 shen'me 无词：旧行为每格只取
    // cands[0]，只出「神么」；改后两种同音切分都要现身，高频在前。
    let (dict, db) = open_dict(
        "homophone",
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('shen','神',90000000,'s',0),
           ('shen','什',70000000,'s',0),
           ('me','么',80000000,'m',0);",
    );
    let got = viterbi_sentences(&dict, &reading(&["shen", "me"]));
    assert_eq!(
        texts(&got),
        vec!["神么".to_string(), "什么".to_string()],
        "同格两个同音词都应成边：实际 {got:?}"
    );
    let _ = fs::remove_file(&db);
}

// ── B4：缺音节占位节点 ─────────────────────────────────────────────────

#[test]
fn placeholder_sentence_is_not_offered_as_candidate() {
    // zzz 在词库中无任何条目（连单字都没有）：穿过 zzz 的路径由占位边兜底
    // 仍拼得出文本，但占位句 text 混着拼音字母（「你好zzz」）、不可上屏，
    // 不得作为候选输出（qingjian has_placeholder 同款门禁）。
    let (dict, db) = open_dict(
        "placeholder",
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('ni','你',1000,'n',0),
           ('hao','好',900,'h',0);",
    );
    let got = viterbi_sentences(&dict, &reading(&["ni", "hao", "zzz"]));
    assert!(
        got.is_empty(),
        "含未收录音节的占位句不得进候选列：实际 {got:?}"
    );
    let _ = fs::remove_file(&db);
}

#[test]
fn partial_placeholder_path_yields_only_clean_segment() {
    // 混合场景：zzz 本身有词条，但另有路径干净（ni+hao）。
    // 只有干净路径可作候选；穿过 zzz 词条的路径不受占位影响。
    let (dict, db) = open_dict(
        "mixed",
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('ni','你',1000,'n',0),
           ('hao','好',900,'h',0),
           ('zzz','啊',1,'z',0);",
    );
    let got = viterbi_sentences(&dict, &reading(&["ni", "hao", "zzz"]));
    // 每个音节都有词条：ni→你 / hao→好 / zzz→啊，唯一组合是 你好啊。
    // 无占位混入（否则会出现拼音字母文本）。
    assert_eq!(
        texts(&got),
        vec!["你好啊".to_string()],
        "全音节有词时按词图组合输出：实际 {got:?}"
    );
    let _ = fs::remove_file(&db);
}

#[test]
fn seed_does_not_leak_into_clean_sentence() {
    // 不变量 a 的占位场景投影：seed 占 dp[0] 时，干净路径（ni+hao）仍可作候选，
    // 且 text/pinyin 恒不含种子部分；穿过未收录音节的占位句仍不得输出。
    let (dict, db) = open_dict(
        "seed",
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('ni','你',1000,'n',0),
           ('hao','好',900,'h',0);",
    );
    let seed = Seed {
        reading: "wo'men".to_string(),
        freq: 509405,
    };
    let blocked = viterbi_sentences_seeded(
        &dict,
        &reading(&["ni", "hao", "zzz"]),
        Some(seed.clone()),
        &mut SpanCache::default(),
    );
    assert!(
        blocked.is_empty(),
        "唯一路径穿过未收录音节，占位句不得输出：实际 {blocked:?}"
    );
    let got = viterbi_sentences_seeded(
        &dict,
        &reading(&["ni", "hao"]),
        Some(seed),
        &mut SpanCache::default(),
    );
    assert_eq!(
        texts(&got),
        vec!["你好".to_string()],
        "干净路径在 seed 下仍须可输出：实际 {got:?}"
    );
    assert!(
        !got[0].pinyin.contains("wo"),
        "候选拼音混入种子音节：{got:?}"
    );
    let _ = fs::remove_file(&db);
}

// ── B2 上限语义：束宽截断 + 代价升序 + 同文本去重 ─────────────────────

#[test]
fn beam_cap_keeps_cheapest_eight() {
    // 4 音节 × 每音节 2 个同音字 = 16 条文本互异切分 > 束宽 8：返回条数
    // 必须被截到 8（上限语义不变），首条恒为全局最低代价组合。
    let (dict, db) = open_dict(
        "cap",
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('ni','你',100000,'n',0),
           ('ni','泥',90000,'n',0),
           ('hao','好',80000,'h',0),
           ('hao','号',70000,'h',0),
           ('shi','世',60000,'s',0),
           ('shi','势',50000,'s',0),
           ('jie','界',40000,'j',0),
           ('jie','戒',30000,'j',0);",
    );
    let got = viterbi_sentences(&dict, &reading(&["ni", "hao", "shi", "jie"]));
    assert_eq!(got.len(), 8, "16 条互异切分必须按束宽截到 8：实际 {got:?}");
    assert_eq!(
        got[0].text, "你好世界",
        "代价升序：最低代价切分居首：{got:?}"
    );
    assert!(
        got.windows(2).all(|w| w[0].text != w[1].text),
        "同文本不得重复：{got:?}"
    );
    let _ = fs::remove_file(&db);
}

// ── 不变量 b / 缓存语义：SpanCache 公开面未被破坏 ────────────────────

#[test]
fn span_cache_api_still_holds() {
    // 缓存热路径与全冷路径结果一致（对拍语义，与 span_cache_test 同款，
    // 钉在 lattice 层）；SpanCache 公开 API（len/is_empty/clear/get_or_compute）
    // 与 MAX_SPAN_CACHE_ENTRIES 仍可编译可用。
    let (dict, db) = open_dict(
        "cache",
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('ni','你',400000,'n',0),
           ('hao','好',390000,'h',0),
           ('ni''hao','你好',500000,'nh',0);",
    );
    let parts = ["ni", "hao"];
    let cold = viterbi_sentences(&dict, &reading(&parts));
    let mut cache = SpanCache::default();
    let warm = viterbi_sentences_seeded(&dict, &reading(&parts), None, &mut cache);
    assert_eq!(
        warm, cold,
        "缓存命中路径不得偏离全冷重算：warm={warm:?} cold={cold:?}"
    );
    assert!(!cache.is_empty(), "查过跨度后缓存不得为空");
    assert!(
        cache.len() <= MAX_SPAN_CACHE_ENTRIES,
        "缓存条数不得超过上限：{}",
        cache.len()
    );
    cache.clear();
    assert!(cache.is_empty(), "clear 后必须为空");
    let _ = fs::remove_file(&db);
}

// ── 回归：原 lattice.rs 内联测试（移入 tests/，按新语义重钉两条）──────

#[test]
fn whole_word_still_beats_split() {
    // 词库有整串词 你好世界 时，它必须仍居首（整词恒胜组合切分）。
    let (dict, db) = open_dict(
        "whole",
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('ni''hao','你好',1000,'nh',0),
           ('shi''jie','世界',800,'sj',0),
           ('wo','我',5000,'w',0),
           ('men','们',3000,'m',0),
           ('ai','爱',2000,'a',0),
           ('ni''hao''shi''jie','你好世界',500,'nh',0),
           ('wo''ai''ni','我爱你',600,'wa',0);",
    );
    let got = viterbi_sentences(&dict, &reading(&["ni", "hao", "shi", "jie"]));
    assert_eq!(
        got.first().map(|c| c.text.as_str()),
        Some("你好世界"),
        "整词必须居首：{got:?}"
    );
    let _ = fs::remove_file(&db);
}

#[test]
fn full_word_beats_single_char_fragments() {
    let (dict, db) = open_dict(
        "frag",
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('ni''hao','你好',1000,'nh',0),
           ('shi''jie','世界',800,'sj',0),
           ('wo','我',5000,'w',0),
           ('men','们',3000,'m',0),
           ('ai','爱',2000,'a',0),
           ('ni''hao''shi''jie','你好世界',500,'nh',0),
           ('wo''ai''ni','我爱你',600,'wa',0);",
    );
    let got = viterbi_sentences(&dict, &reading(&["wo", "ai", "ni"]));
    assert_eq!(
        got.first().map(|c| c.text.as_str()),
        Some("我爱你"),
        "整词必须居首：{got:?}"
    );
    let _ = fs::remove_file(&db);
}

#[test]
fn single_syllable_input_yields_no_sentence() {
    let (dict, db) = open_dict(
        "single",
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('ni','你',1000,'n',0);",
    );
    assert!(viterbi_sentences(&dict, &reading(&["ni"])).is_empty());
    let _ = fs::remove_file(&db);
}

#[test]
fn all_syllables_missing_yields_no_candidate() {
    // 词库全 miss：所有路径都得靠占位边兜底，而占位句 text 是拼音字母、
    // 不可上屏，故候选列为空（等价于旧 test_viterbi_no_match_fallback 的空返回）。
    let (dict, db) = open_dict("nomatch", "");
    let got = viterbi_sentences(&dict, &reading(&["xxx", "yyy"]));
    assert!(got.is_empty(), "全 miss 的占位句不得进候选列：实际 {got:?}");
    let _ = fs::remove_file(&db);
}

#[test]
fn partial_missing_syllable_yields_no_candidate() {
    // 前段有词、末音节全 miss：唯一路径需占位边补末音节，占位句不可上屏，
    // 候选列为空（等价于旧 test_viterbi_partial_match 的空返回）。
    let (dict, db) = open_dict(
        "partial",
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('ni''hao','你好',1000,'nh',0);",
    );
    let got = viterbi_sentences(&dict, &reading(&["ni", "hao", "xxx"]));
    assert!(got.is_empty(), "缺末音节的占位句不得进候选列：实际 {got:?}");
    let _ = fs::remove_file(&db);
}
