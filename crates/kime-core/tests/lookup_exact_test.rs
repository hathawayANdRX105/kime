//! `Dict::lookup` / `FstStore::lookup_exact` 的精确匹配语义。
//!
//! `lookup` 长期用 `lookup_prefix(reading, "", limit)` 实现：那是前缀区间查询，
//! `increment_prefix("ni'hao")` = `"ni'hap"`，区间 `["ni'hao", "ni'hap")` 会带上
//! `ni'hao'shi'jie` 这类更长的 key。v2 的「攒满 limit 即 break」早停掩盖了它；
//! 换成真正的全局 top-k 归并后，更长更高频的词浮到前面，Viterbi 拿 2 音节跨度
//! 得到 4 音节的词，格子里的词互相重叠，整句退化成乱码。

use std::fs;
use std::path::Path;

use kime_core::builder::build;
use kime_core::dict::{Candidate, Dict};

/// `ni'hao` 一条低频词，`ni'hao'shi'jie` 一条高频词：前缀查询会把后者也捞进来。
fn seeded(dir: &Path) -> Dict {
    let yaml = dir.join("seed.yaml");
    fs::write(
        &yaml,
        "...\n你好\tni hao\t100\n你好世界\tni hao shi jie\t99999\n世界\tshi jie\t5000\n",
    )
    .unwrap();
    let db = dir.join("dict.sqlite3");
    let bin = dir.join("dict.bin");
    let mut seed = Dict::open(&db).unwrap();
    seed.import(&yaml).unwrap();
    drop(seed);
    build(&db, &bin).unwrap();
    Dict::open(&db).unwrap()
}

fn texts(hits: &[Candidate]) -> Vec<&str> {
    hits.iter().map(|c| c.text.as_str()).collect()
}

#[test]
fn lookup_excludes_longer_keys_sharing_the_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let d = seeded(dir.path());

    let hits = d.lookup(&["ni".into(), "hao".into()], 10).unwrap();
    assert_eq!(
        texts(&hits),
        vec!["你好"],
        "精确查询只能返回该读音的词，不得带上 ni'hao'shi'jie"
    );
}

#[test]
fn lookup_prefix_still_includes_longer_keys() {
    let dir = tempfile::tempdir().unwrap();
    let d = seeded(dir.path());

    // 对照：前缀查询就该看到长词（用户还在打字，ni'hao 是 ni'hao'shi'jie 的前缀）
    let hits = d.lookup_prefix(&["ni".into()], "hao", 10).unwrap();
    assert_eq!(
        texts(&hits),
        vec!["你好世界", "你好"],
        "前缀查询应含更长的 key，且按 freq 降序"
    );
}
