//! 回归：v3 惰性解码 + top-k 归并不得丢高频词。
//!
//! v2 在 `lookup_prefix` 里「收集满 limit 即 break」：第一个 key 的块解满 limit 条后，
//! 后面 key 里 freq 更高的词被整块跳过。v3 改为「每块至多解 limit 条 + 块头剪枝的
//! 全程归并」，必须返回区间全局 freq 最高的 limit 条（tests 同时覆盖热表与全扫两条路径）。

use std::fs;
use std::path::Path;

use kime_core::builder::build;
use kime_core::dict::{Candidate, Dict};
use kime_core::store::FstStore;

/// 种子：key `a'la` 一块 10 条（单块 > limit=5，freq 10..100）；key `a'zi` 两条高频（9000/8000）。
/// FST 序里 `a'la` < `a'zi`：v2 早停先解满 `a'la` 块即 break，永远看不到 9000/8000。
fn seed_and_open(dir: &Path) -> FstStore {
    let mut body = String::from("...\n");
    for freq in (1..=10u32).rev() {
        body.push_str(&format!("低{freq}\ta la\t{freq}0\n"));
    }
    body.push_str("高甲\ta zi\t9000\n高乙\ta zi\t8000\n");
    let yaml = dir.join("seed.yaml");
    fs::write(&yaml, body).unwrap();
    let db = dir.join("dict.sqlite3");
    let bin = dir.join("dict.bin");
    let mut seed = Dict::open(&db).unwrap();
    assert_eq!(seed.import(&yaml).unwrap(), 12, "种子数据未完整入库");
    drop(seed);
    build(&db, &bin).unwrap();
    FstStore::open(&bin).unwrap()
}

fn texts(hits: &[Candidate]) -> Vec<&str> {
    hits.iter().map(|c| c.text.as_str()).collect()
}

#[test]
fn scan_merge_returns_global_topk_across_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let store = seed_and_open(dir.path());

    // 全扫归并路径：joined 含 `'`，range [a', aa) 命中 a'la + a'zi 两个 key。
    let hits = store.lookup_prefix(&[], "a'", 5);
    assert_eq!(
        texts(&hits),
        vec!["高甲", "高乙", "低10", "低9", "低8"],
        "高频词不得被前面 key 的低频块挤掉"
    );
    assert_eq!(hits[0].pinyin, "a'zi");
    assert_eq!(hits[2].pinyin, "a'la");

    // 单块超 limit 且 limit 在块内截断：块内前 5 即该块 freq 最高 5 条。
    let only_la = store.lookup_prefix(&["a".into()], "la", 5);
    assert_eq!(texts(&only_la), vec!["低10", "低9", "低8", "低7", "低6"]);
}

#[test]
fn topk_table_matches_full_merge() {
    let dir = tempfile::tempdir().unwrap();
    let store = seed_and_open(dir.path());

    // 热表路径（joined 无 `'`）与全扫结果必须一致——表就是同一 top-k 的预计算。
    let via_table = store.lookup_prefix(&[], "a", 5);
    assert_eq!(
        texts(&via_table),
        vec!["高甲", "高乙", "低10", "低9", "低8"]
    );
    let via_scan = store.lookup_prefix(&[], "a'", 5);
    assert_eq!(texts(&via_table), texts(&via_scan));

    // limit > 桶容量：走全扫，结果仍完整。
    let all = store.lookup_prefix(&[], "a", 100);
    assert_eq!(all.len(), 12);
    assert!(all.windows(2).all(|w| w[0].freq >= w[1].freq));
}

#[test]
fn abbrev_topk_spans_multiple_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let store = seed_and_open(dir.path());

    // abbrev "a"：单字母走热表（与 1 字母前缀同一 key 集）；abbrev "al" 走偏移表扫。
    let hits = store.lookup_abbrev("a", 5);
    assert_eq!(texts(&hits), vec!["高甲", "高乙", "低10", "低9", "低8"]);
    let al = store.lookup_abbrev("al", 10);
    assert_eq!(al.len(), 10, "a'la 块应整块取回（limit 未触发剪枝）");
    assert_eq!(al[0].text, "低10");
    let az = store.lookup_abbrev("az", 10);
    assert_eq!(texts(&az), vec!["高甲", "高乙"]);
}

/// 守卫 builder 侧有界堆：桶容量 64，桶内候选 > 64 且高频词排在词序尾部时，
/// 热表仍须保留全局前 64（堆顶必须是当前最差者；置换方向反了的实现会只留首次遇到的 64 条）。
#[test]
fn oversized_bucket_keeps_global_topk_in_table() {
    let dir = tempfile::tempdir().unwrap();
    let mut body = String::from("...\n");
    for freq in 1..=70u32 {
        body.push_str(&format!("常{freq}\tba\t{freq}\n"));
    }
    body.push_str("稀9999\tba zi\t9999\n");
    let yaml = dir.path().join("seed.yaml");
    fs::write(&yaml, body).unwrap();
    let db = dir.path().join("dict.sqlite3");
    let bin = dir.path().join("dict.bin");
    let mut seed = Dict::open(&db).unwrap();
    assert_eq!(seed.import(&yaml).unwrap(), 71);
    drop(seed);
    build(&db, &bin).unwrap();
    let store = FstStore::open(&bin).unwrap();

    // 前缀 "ba" 的桶收 71 条候选（key ba 的 70 条 freq1..70 + key ba'zi 的 freq9999），
    // limit=50 走热表：期望 9999 置顶 + freq 70..22 共 50 条。
    let hits = store.lookup_prefix(&[], "ba", 50);
    assert_eq!(hits[0].text, "稀9999");
    assert_eq!(hits.len(), 50);
    let expected: Vec<String> = std::iter::once("稀9999".to_string())
        .chain((22..=70).rev().map(|f| format!("常{f}")))
        .collect();
    assert_eq!(
        hits.iter().map(|c| c.text.clone()).collect::<Vec<_>>(),
        expected,
        "宽前缀热表必须是全局 top-K，而非首见 64 条"
    );

    // 恰好取满桶容量：第 64 名 = freq 8。
    let full = store.lookup_prefix(&[], "ba", 64);
    assert_eq!(full.len(), 64);
    assert_eq!(full[63].text, "常8");
}
