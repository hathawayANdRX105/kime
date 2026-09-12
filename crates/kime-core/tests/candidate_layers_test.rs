//! 两层候选契约（工单第 1+2 条）：
//! 1. 音节边界——tail 为空（音节打完）时，补全只许跨 `'` 边界：`min` 绝不补全出
//!    `ming`（用户还得往同一音节里塞字母），`min'xxx` 才合法。
//! 2. 分层——精确命中先于补全，哪怕补全词频率高一个数量级（民 90 万 vs 明 286 万）。
//! 3. FST 路径与纯 SQLite 内存索引回退路径必须同语义。

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

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

fn tmp_path(prefix: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "kime_layers_{}_{}_{}.sqlite",
        prefix,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

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

/// 打 `min` 首屏必须是 min 自己的字（民/敏/…），明/名/命不得出现在前五，
/// 但在 50 条列表里仍可达（tail 未打完 = 还允许继续补全）。
fn exact_layer_leads_despite_lower_freq(d: &Dict) {
    let hits = d.lookup_prefix(&[], "min", 50).unwrap();
    assert_eq!(
        texts(&hits[..5]),
        vec!["民", "敏", "抿", "悯", "闵"],
        "层一（key=min）必须先占住前五，哪怕明/名频率高一个数量级"
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
    exact_layer_leads_despite_lower_freq(&d);
}

#[test]
fn sqlite_fallback_paths_honor_the_same_contract() {
    let dir = tempfile::tempdir().unwrap();
    // 不 build dict.bin：store = None，走纯内存 index 回退路径
    let d = sqlite_dict(dir.path());
    open_tail_still_reaches_longer_syllables(&d);
    exact_layer_leads_despite_lower_freq(&d);
    let hits = d.lookup_prefix(&["min".into()], "", 20).unwrap();
    let got = texts(&hits);
    assert!(got.contains(&"民") && got.contains(&"迷你"), "{got:?}");
    assert!(
        !got.contains(&"明") && !got.contains(&"名") && !got.contains(&"命"),
        "回退路径与 FST 路径必须同谓词：{got:?}"
    );
}

/// 用户词不许破层：把 `ming` 的明学成用户词（freq 再高），打 min 的前五仍是 min 的字。
#[test]
fn user_word_cannot_jump_layers() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = fst_dict(dir.path());
    d.learn(&["ming".into()], "明").unwrap();
    let hits = d.lookup_prefix(&[], "min", 50).unwrap();
    assert_eq!(
        texts(&hits[..5]),
        vec!["民", "敏", "抿", "悯", "闵"],
        "用户词明（层二成员）不得插队进层一"
    );
    // 但它在层二里以学后频率出现
    assert!(
        hits.iter().any(|c| c.text == "明" && c.freq == 2864493),
        "{:?}",
        texts(&hits)
    );
}
