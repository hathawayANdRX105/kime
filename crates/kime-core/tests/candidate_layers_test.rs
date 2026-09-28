//! 两层候选契约（工单第 1+2 条）：
//! 1. 音节边界——tail 为空（音节打完）时，补全只许跨 `'` 边界：`min` 绝不补全出
//!    `ming`（用户还得往同一音节里塞字母），`min'xxx` 才合法。
//! 2. 分层——精确命中先于补全，哪怕补全词频率高一个数量级（民 90 万 vs 明 286 万）。
//! 3. FST 路径与纯 SQLite 内存索引回退路径必须同语义。

use std::fs;
use std::path::Path;

use kime_core::builder::build;
use kime_core::dict::{Candidate, Dict};

/// 复刻线上事故数据形态：`min` 精确块 5 字、`mi` 一个超高频字；`ming` 三个超高频字；`min'ni` 跨边界补全。
const SEED: &str = "...\n\
民\tmin\t902697\n\
敏\tmin\t196615\n\
抿\tmin\t21329\n\
悯\tmin\t19771\n\
闵\tmin\t17114\n\
米\tmi\t7000000\n\
明\tming\t2864492\n\
名\tming\t1804559\n\
命\tming\t1368062\n\
迷你\tmin ni\t133000\n";

fn texts(hits: &[Candidate]) -> Vec<&str> {
    hits.iter().map(|c| c.text.as_str()).collect()
}

/// 纯 SQLite 内存索引（无 dict.bin → store 回退路径）。
fn sqlite_dict(dir: &Path) -> Dict {
    let db = dir.join("dict.sqlite3");
    let yaml = dir.join("seed.yaml");
    fs::write(&yaml, SEED).unwrap();
    let mut d = Dict::open(&db).unwrap();
    d.import(&yaml).unwrap();
    d
}

/// 同一份数据编译出 dict.bin 后的 FST 路径。
fn fst_dict(dir: &Path) -> Dict {
    let db = dir.join("dict.sqlite3");
    let bin = dir.join("dict.bin");
    let d = sqlite_dict(dir);
    drop(d);
    build(&db, &bin).unwrap();
    Dict::open(&db).unwrap()
}

/// tail 为空（音节已打完）：候选集 = 精确块 + `joined'` 边界区间，`ming` 必须消失。
fn completed_syllable_never_completes_into_longer_syllable(d: &Dict) {
    let hits = d.lookup_prefix(&["min".into()], "", 20).unwrap();
    let got = texts(&hits);
    assert!(got.contains(&"民") && got.contains(&"敏"), "{got:?}");
    assert!(got.contains(&"迷你"), "min'ni 是合法跨边界补全，{got:?}");
    assert!(
        !got.contains(&"明") && !got.contains(&"名") && !got.contains(&"命"),
        "tail 为空时 ming 不是 min 的补全：{got:?}"
    );
}

/// tail 非空（还在打）：开区间行为不许砍掉——`mi` 仍要能碰到 min/ming 的字，
/// 同时精确命中（米）仍站队首。
fn open_tail_still_reaches_longer_syllables(d: &Dict) {
    let hits = d.lookup_prefix(&[], "mi", 50).unwrap();
    let got = texts(&hits);
    assert_eq!(
        got.first().copied(),
        Some("米"),
        "层一精确命中必须置顶：{got:?}"
    );
    assert!(
        got.contains(&"民") && got.contains(&"明"),
        "开区间必须仍够得着 min/ming：{got:?}"
    );
}

/// #87 交错排序（替代旧的「块拼接」验收）：`min` 输入时，裸频到达语料高频档的
/// 补全字（明 286 万 / 名 180 万 / 命 136 万）凭 eff 自然插到精确字（民 90 万
/// +EXACT_BONUS≈120 万）之前；精确加分（USER_BOOST 量级）仍保证 min 块整体
/// 压住迷你（13 万）等更弱的补全。补全字可达性由 contains 检查保持。
fn exact_layer_interleaves_with_high_freq_completions(d: &Dict) {
    let hits = d.lookup_prefix(&[], "min", 50).unwrap();
    assert_eq!(
        texts(&hits[..5]),
        vec!["明", "名", "命", "民", "敏"],
        "交错：补全字 eff 到语料高频档时插到精确字前，精确字带加分压住弱补全：{:?}",
        texts(&hits)
    );
    assert!(
        hits.iter().any(|c| c.text == "明"),
        "开区间状态补全必须仍可达：{:?}",
        texts(&hits)
    );
}

#[test]
fn fst_paths_honor_the_layering_contract() {
    let dir = tempfile::tempdir().unwrap();
    let d = fst_dict(dir.path());
    // 种子必须真的进了 FST，否则下面三条契约检查会空过
    assert_eq!(
        d.lookup(&["min".into()], 10).unwrap().len(),
        5,
        "SEED 的 min 块未完整入库"
    );
    completed_syllable_never_completes_into_longer_syllable(&d);
    open_tail_still_reaches_longer_syllables(&d);
    exact_layer_interleaves_with_high_freq_completions(&d);
}

#[test]
fn sqlite_fallback_paths_honor_the_same_contract() {
    let dir = tempfile::tempdir().unwrap();
    // 不 build dict.bin：store = None，走纯内存 index 回退路径
    let d = sqlite_dict(dir.path());
    open_tail_still_reaches_longer_syllables(&d);
    exact_layer_interleaves_with_high_freq_completions(&d);
    let hits = d.lookup_prefix(&["min".into()], "", 20).unwrap();
    let got = texts(&hits);
    assert!(got.contains(&"民") && got.contains(&"迷你"), "{got:?}");
    assert!(
        !got.contains(&"明") && !got.contains(&"名") && !got.contains(&"命"),
        "回退路径与 FST 路径必须同谓词：{got:?}"
    );
}

/// 用户词不破层（#87 版）：把 `ming` 的明学成用户词后，它凭学后 eff（语料 286 万
/// + USER_BOOST 30 万）合法地排到交错队列首位（`min` 的补全字本来就够格插进精确
/// 字之间），但层归属不变：不得复制进层一、学后频率语义保留。
#[test]
fn user_word_cannot_jump_layers() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = fst_dict(dir.path());
    d.learn(&["ming".into()], "明").unwrap();
    let hits = d.lookup_prefix(&[], "min", 50).unwrap();
    assert!(
        hits.iter().any(|c| c.text == "明" && c.freq == 2864493),
        "学后频率语义必须保留：{:?}",
        texts(&hits)
    );
    assert!(
        hits.iter().filter(|c| c.text == "明").count() == 1,
        "用户词明不得跨层复制成两条：{:?}",
        texts(&hits)
    );
}
