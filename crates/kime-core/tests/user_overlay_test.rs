//! FST 模式下的用户词 overlay。
//!
//! 用户词曾经靠每次按键跑 `WHERE user = 1` 的 SQL 捞（无可用索引，实测单键 10–57ms），
//! 现在全量常驻内存。overlay 的危险在于「与 SQLite 失同步」：learn 之后必须立刻查得到，
//! 且词库词升级为用户词时频率必须是权威值而非凭空的 1。

use std::fs;
use std::path::Path;

use kime_core::builder::build;
use kime_core::dict::{Candidate, Dict};

/// 灌种子词 → build dict.bin → 重开（`Dict::open` 认同目录下的 dict.bin，store 生效）。
fn fst_dict(dir: &Path, seed: &str) -> Dict {
    let yaml = dir.join("seed.yaml");
    fs::write(&yaml, format!("...\n{seed}")).unwrap();
    let db = dir.join("dict.sqlite3");
    let bin = dir.join("dict.bin");
    let mut seed_dict = Dict::open(&db).unwrap();
    seed_dict.import(&yaml).unwrap();
    drop(seed_dict);
    build(&db, &bin).unwrap();
    Dict::open(&db).unwrap()
}

fn texts(hits: &[Candidate]) -> Vec<&str> {
    hits.iter().map(|c| c.text.as_str()).collect()
}

#[test]
fn learned_word_is_visible_without_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = fst_dict(dir.path(), "你好\tni hao\t5000\n");

    // 全新用户词：dict.bin 里没有，只能从 overlay 出
    d.learn(&["kai".into(), "ming".into()], "凯明").unwrap();

    let exact = d.lookup(&["kai".into(), "ming".into()], 10).unwrap();
    assert!(
        texts(&exact).contains(&"凯明"),
        "learn 后精确查询应立即命中，实际 {:?}",
        texts(&exact)
    );

    // 前缀查询与缩写查询是另外两条 overlay 路径，同样必须命中
    let prefix = d.lookup_prefix(&["kai".into()], "m", 10).unwrap();
    assert!(
        texts(&prefix).contains(&"凯明"),
        "前缀查询应命中用户词，实际 {:?}",
        texts(&prefix)
    );
    let abbrev = d.lookup_abbrev("km", 10).unwrap();
    assert!(
        texts(&abbrev).contains(&"凯明"),
        "缩写查询应命中用户词，实际 {:?}",
        texts(&abbrev)
    );
}

#[test]
fn promoted_dictionary_word_keeps_its_real_frequency() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = fst_dict(dir.path(), "你好\tni hao\t5000\n");

    // 学「你好」：它本是词库词（freq 5000），UPDATE 把 user 置 1、freq 变 5001。
    // 此后它只从 overlay 出（overlay 覆盖同文本的基底候选），凭空写 freq=1 会让
    // 这个词在任何与高频词同列的查询里沉底。断言频率本身，不是排序位置。
    d.learn(&["ni".into(), "hao".into()], "你好").unwrap();

    let hits = d.lookup(&["ni".into(), "hao".into()], 10).unwrap();
    let hit = hits
        .iter()
        .find(|c| c.text == "你好")
        .expect("你好应在候选里");
    assert_eq!(
        hit.freq, 5001,
        "升级为用户词后频率应是词库频率+1，实际 {}",
        hit.freq
    );
}
#[test]
fn top_user_reads_the_overlay_under_fst() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = fst_dict(dir.path(), "你好\tni hao\t5000\n");
    d.learn(&["kai".into(), "ming".into()], "凯明").unwrap();

    // FST 模式下内存 index 是空的：top_user 曾因此永远返回空。
    let top = d.top_user(10).unwrap();
    assert!(
        texts(&top).contains(&"凯明"),
        "top_user 应看到用户词，实际 {:?}",
        texts(&top)
    );
}
