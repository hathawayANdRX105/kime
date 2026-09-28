//! #87 交错排序契约：词候选与单字/补全候选统一分数池。
//!
//! 旧「块拼接」语义的替代验收：
//! 1. 精确命中词（你好，语料高频）整体压住弱补全（你好吗 1 万级）——「我们去」
//!    不被「我们确信」压住的同型守卫；
//! 2. 首音节高频单字（你/好）按 eff 参与插位，不再无条件垫底；
//! 3. 双条路径（FST / 纯 SQLite）同序。

use std::fs;
use std::path::Path;

use kime_core::builder::build;
use kime_core::dict::Candidate;
use kime_core::dict::Dict;

/// 「ni」事故镜像：你好 = 高频精确词（50 万）；你好吗 = 弱补全（1 万）；
/// 你/好 = 中等频率单字（你 30 万、好 15 万）。
const SEED: &str = "...\n你好\tni hao\t500000\n\
                    你好吗\tni hao ma\t10000\n\
                    你\tni\t300000\n\
                    好\thao\t150000\n";

fn texts(hits: &[Candidate]) -> Vec<&str> {
    hits.iter().map(|c| c.text.as_str()).collect()
}

fn sqlite_dict(dir: &Path) -> Dict {
    let db = dir.join("dict.sqlite3");
    let yaml = dir.join("seed.yaml");
    fs::write(&yaml, SEED).unwrap();
    let mut d = Dict::open(&db).unwrap();
    d.import(&yaml).unwrap();
    d
}

fn fst_dict(dir: &Path) -> Dict {
    let db = dir.join("dict.sqlite3");
    let bin = dir.join("dict.bin");
    let d = sqlite_dict(dir);
    drop(d);
    build(&db, &bin).unwrap();
    Dict::open(&db).unwrap()
}

/// `ni` 收口（tail 空）：精确项 = 你（30 万 + EXACT_BONUS 30 万 = 60 万）压住
/// 补全区 你好（50 万）/ 你好吗（1 万）；单字不被挤出列表。
fn completed_single_bears_bonus_over_completions(d: &Dict) {
    let hits = d.lookup_prefix(&["ni".into()], "", 50).unwrap();
    let got = texts(&hits);
    assert_eq!(
        got.first().copied(),
        Some("你"),
        "精确单字带加分必须站队首：{got:?}"
    );
    assert!(got.contains(&"你好吗"), "弱补全仍可达：{got:?}");
}

/// tail 非空（还在打 hao）：你好（50 万 + 30 万加分）压住 你好吗（1 万）与
/// 好（15 万）——#87 核心用例（打「nihao」你好永远排前）。
fn in_progress_tail_keeps_exact_word_ahead(d: &Dict) {
    let hits = d.lookup_prefix(&["ni".into()], "hao", 50).unwrap();
    let got = texts(&hits);
    assert_eq!(
        got.first().copied(),
        Some("你好"),
        "进行中补全区里精确词仍凭加分压住弱补全：{got:?}"
    );
}

#[test]
fn fst_path_interleave_contract() {
    let dir = tempfile::tempdir().unwrap();
    let d = fst_dict(dir.path());
    completed_single_bears_bonus_over_completions(&d);
    in_progress_tail_keeps_exact_word_ahead(&d);
}

#[test]
fn sqlite_path_interleave_contract() {
    let dir = tempfile::tempdir().unwrap();
    let d = sqlite_dict(dir.path());
    completed_single_bears_bonus_over_completions(&d);
    in_progress_tail_keeps_exact_word_ahead(&d);
}
