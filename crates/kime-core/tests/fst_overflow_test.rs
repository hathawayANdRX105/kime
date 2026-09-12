//! 回归：dict.bin 的每键候选计数曾因 u16 溢出回绕（rime-ice `pinyin='100'` 组
//! 980,961 条 → 63457），导致 `FstStore::open` 扫描 values 区错位、blocks 截断，
//! 除首块外所有 lookup 返回空。计数改为 u32 后，open 还必须对错位/旧格式报 Err，
//! 以便 `Dict::open` 回退 SQLite 内存索引。

use std::fs;
use std::path::{Path, PathBuf};

use kime_core::builder::build;
use kime_core::dict::Dict;
use kime_core::store::FstStore;

/// 超过 u16 上限（65536）的候选组规模。
const BIG: usize = 70_000;

fn seed_yaml(path: &Path, body: &str) {
    let mut text = String::from("...\n");
    text.push_str(body);
    fs::write(path, text).unwrap();
}

/// 大组拼音（`a'ba`）在 FST 序里排在 `ni'hao` 之前：一旦它溢出回绕，
/// 后续所有键的 block 偏移全部错位——正是线上观察到的症状。
fn big_body() -> String {
    let mut body = String::new();
    for i in 0..BIG {
        body.push_str(&format!("码{i:05}\ta ba\t{}\n", BIG - i));
    }
    body.push_str("你好\tni hao\t5000\n拟好\tni hao\t100\n");
    body
}

/// 建 SQLite 词库并编译 dict.bin；`rows` 用于确认种子数据一条都没被 INSERT OR IGNORE 吞掉。
fn build_from_body(dir: &Path, body: &str, rows: usize) -> (PathBuf, PathBuf, u64) {
    let db = dir.join("dict.sqlite3");
    let bin = dir.join("dict.bin");
    let yaml = dir.join("seed.yaml");
    seed_yaml(&yaml, body);
    let mut seed = Dict::open(&db).unwrap();
    assert_eq!(seed.import(&yaml).unwrap(), rows, "种子数据未完整入库");
    drop(seed);
    let total = build(&db, &bin).unwrap();
    (db, bin, total)
}

#[test]
fn oversized_pinyin_group_keeps_every_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let (_, bin, total) = build_from_body(dir.path(), &big_body(), BIG + 2);
    assert_eq!(
        total as usize,
        BIG + 2,
        "total_candidates 必须是真实条数，不能是截断后的计数"
    );

    let store = FstStore::open(&bin).unwrap();

    // 大组本身：count 字段无回绕 → 70,000 条全部取回，且按 freq 降序
    let hits = store.lookup_prefix(&["a".into()], "ba", BIG + 2);
    assert_eq!(hits.len(), BIG, "u16 回绕会让大组只剩 4464 条");
    assert_eq!(hits[0].text, "码00000");
    assert_eq!(hits[BIG - 1].text, "码69999");
    assert!(hits.iter().all(|c| c.pinyin == "a'ba"));

    // 大组之后的普通键仍须命中（旧 bug 里它的 block 偏移已错位）
    let ni = store.lookup_prefix(&["ni".into()], "hao", 10);
    assert_eq!(
        ni.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        vec!["你好", "拟好"]
    );

    // 声母缩写走的是另一条索引路径，同样不得被大组带偏
    let nh = store.lookup_abbrev("nh", 10);
    assert!(nh.iter().any(|c| c.text == "你好"));
}

#[test]
fn open_rejects_truncated_and_legacy_dict_bin() {
    let dir = tempfile::tempdir().unwrap();
    let body = "你好\tni hao\t5000\n拟好\tni hao\t100\n什么\tshen me\t8000\n";
    let (db, bin, _) = build_from_body(dir.path(), body, 3);
    assert!(FstStore::open(&bin).is_ok(), "健康的 v3 词库必须能打开");

    // (a) 文件被截尾：各区声明长度与文件大小不符 → Err（v2 时代靠全量扫描发现，v3 由区界校验发现）
    let mut bytes = fs::read(&bin).unwrap();
    let cut = bytes.len() - 8;
    bytes.truncate(cut);
    let truncated = dir.path().join("truncated.bin");
    fs::write(&truncated, &bytes).unwrap();
    assert!(
        FstStore::open(&truncated).is_err(),
        "截断的 dict.bin 必须报 Err"
    );

    // (b) 旧格式文件：v3 的索引区不落盘就无法重建，版本门禁必须拒绝 v1/v2，
    //     不能按新头部误读
    let intact = fs::read(&bin).unwrap();
    for legacy_version in [1u32, 2] {
        let mut bytes = intact.clone();
        bytes[4..8].copy_from_slice(&legacy_version.to_le_bytes());
        let legacy = dir.path().join(format!("legacy{legacy_version}.bin"));
        fs::write(&legacy, &bytes).unwrap();
        assert!(
            FstStore::open(&legacy).is_err(),
            "旧格式 (v{legacy_version}) dict.bin 必须报 Err 并触发回退"
        );
    }

    // 回退路径真的可用：坏 dict.bin 摆在 db 旁边，Dict 仍从 SQLite 查出候选
    fs::copy(&dir.path().join("legacy2.bin"), &bin).unwrap();
    let dict = Dict::open(&db).unwrap();
    let hits = dict.lookup_prefix(&["ni".into()], "hao", 10).unwrap();
    assert_eq!(
        hits.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        vec!["你好", "拟好"]
    );
}
