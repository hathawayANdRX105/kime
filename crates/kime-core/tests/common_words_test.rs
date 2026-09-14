//! 常用词覆盖（第五轮反馈第 1 条）：不穷举词库，用精选高频词锁住输入主链路。
//!
//! fixture = `tests/fixtures/common_words.tsv`（`词\t拼音`，音节用 `'` 连接）：
//! 252 词 = 从语料 dump（线上词库词频 top）剔除论坛/法律残渣后的真常用词
//! + 手补日常高频词。断言四条路：
//! - 全拼：拼音字母串喂 Engine（ziranma 关闭，page_size 8），词必须进第一页；
//! - 双拼：每音节按 ziranma 表编成两键，词同样必须进第一页；
//! - 前缀补全（≥4 音节）：只喂前 2 音节，词必须出现在候选任意层（可达即可）；
//! - 分词：segment(拼音连写) 必须包含该词的正确音节序列（2+3/3+2 场景锁分词，不锁排序）。
//!
//! 性能护栏：两条主路径全量跑完 < 10s（debug 构建），防查询路径整体劣化。

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::{Engine, Key, Outcome};
use kime_pinyin::segment;
use kime_shuangpin::{Scheme, Table};
use std::collections::HashMap;
use std::fs;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const FIXTURE: &str = include_str!("fixtures/common_words.tsv");
const PAGE: usize = 8;

fn words() -> Vec<(String, Vec<String>)> {
    FIXTURE
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let (w, p) = l.split_once('\t').expect("fixture 行必须是 词\\t拼音");
            (w.to_string(), p.split('\'').map(str::to_string).collect())
        })
        .collect()
}

/// 词频按行号递减：库内排名无平票，断言不受 sqlite 行序影响。
fn yaml() -> String {
    let mut s = String::from("---\nname: common-words\n...\n");
    for (i, line) in FIXTURE.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let (w, p) = line.split_once('\t').unwrap();
        s.push_str(&format!(
            "{w}\t{}\t{}\n",
            p.replace('\'', " "),
            10_000_000i64 - i as i64 * 1000
        ));
    }
    s
}

fn engine(tag: &str, shuangpin: Option<Scheme>) -> Engine {
    let dir = std::env::temp_dir().join(format!(
        "kime_commonwords_{tag}_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let y = dir.join("words.yaml");
    fs::write(&y, yaml()).unwrap();
    let db = dir.join("dict.sqlite3");
    let mut dict = Dict::open(&db).unwrap();
    let n = dict.import(&y).unwrap();
    assert_eq!(n, FIXTURE.lines().filter(|l| !l.trim().is_empty()).count());
    Engine::new(
        dict,
        Config {
            dict_path: db.to_string_lossy().to_string(),
            shuangpin,
            page_size: PAGE,
            ..Config::default()
        },
    )
}

fn k(ch: char) -> Key {
    Key {
        ch: Some(ch),
        code: 0,
        shift: false,
        ctrl: false,
        alt: false,
    }
}

fn esc(e: &mut Engine) {
    e.key(Key {
        ch: None,
        code: 1, // KEY_ESC
        shift: false,
        ctrl: false,
        alt: false,
    });
}

fn type_all(e: &mut Engine, keys: &str) {
    for c in keys.chars() {
        assert_eq!(
            e.key(k(c)),
            Outcome::Consumed,
            "字母 {c:?} 必须进组合（当前键串 {keys:?}）"
        );
    }
}

fn page_texts(e: &Engine) -> Vec<String> {
    e.candidates()
        .iter()
        .take(PAGE)
        .map(|c| c.text.clone())
        .collect()
}

/// 音节 → ziranma 两键码。反向映射直接从引擎用的同一张表推导
/// （遍历 26×26 键对解码），不另立第二份码表，杜绝漂移。
fn ziranma_map() -> HashMap<String, String> {
    let t = Table::new(Scheme::Ziranma);
    let mut m = HashMap::new();
    for a in b'a'..=b'z' {
        for b in b'a'..=b'z' {
            let pair = [a as char, b as char].iter().collect::<String>();
            if let Ok(syls) = t.to_syllables(&pair) {
                if syls.len() == 1 {
                    m.entry(syls[0].clone()).or_insert(pair);
                }
            }
        }
    }
    m
}

fn first_page(word: &str, keys: &str, e: &mut Engine) -> Option<String> {
    esc(e);
    type_all(e, keys);
    let page = page_texts(e);
    if page.iter().any(|t| t == word) {
        None
    } else {
        Some(format!("{word} [{keys}] 首{PAGE}候选 {page:?}"))
    }
}

/// 已确认的引擎缺陷（第五轮报告，src/ 不在本工单范围）：主路径对这两个词跳过断言，
/// 由 `known_bugs_are_still_locked` 锁住缺陷仍然存在。修复后锁测试变红 → 删掉这里的
/// 跳过、恢复逐词断言。
/// - 蛋糕：segment("dangao") 首选 ["dang","ao"]，而 refresh_full_pinyin 只查
///   `segs.first()`（engine.rs:604），正确切分 ["dan","gao"] 永远轮不到 → 全拼 0 候选；
/// - 澳大利亚：ziranma 表 y 系韵母缺 "ya"（也缺 "yo"），音节无法编成两键 → 双拼不可达。
const KNOWN_FULL_BUG: &str = "蛋糕";
const KNOWN_SP_BUG: &str = "澳大利亚";

/// 全拼 + 双拼两条主路径：252 词逐词断言，收集全部失败一次报完
/// （词失败就是要暴露的 bug，别在第一条 assert 就熄火），末尾带性能护栏。
#[test]
fn full_and_shuangpin_hit_first_page() {
    let t0 = Instant::now();
    let ws = words();
    let enc = ziranma_map();
    let mut full = engine("full", None);
    let mut sp = engine("sp", Some(Scheme::Ziranma));
    let mut fails = Vec::new();
    let mut skip_full = 0;
    let mut skip_sp = 0;
    for (w, syls) in &ws {
        if w == KNOWN_FULL_BUG {
            skip_full += 1;
        } else if let Some(m) = first_page(w, &syls.concat(), &mut full) {
            fails.push(format!("全拼: {m}"));
        }
        let unenc: Vec<&String> = syls.iter().filter(|s| !enc.contains_key(*s)).collect();
        if !unenc.is_empty() {
            if w == KNOWN_SP_BUG && unenc.iter().all(|s| s.as_str() == "ya") {
                skip_sp += 1;
            } else {
                fails.push(format!("双拼: 词 {w} 的音节 {unenc:?} 在 ziranma 表无编码"));
            }
            continue;
        }
        let keys: String = syls.iter().map(|s| enc[s].clone()).collect();
        if let Some(m) = first_page(w, &keys, &mut sp) {
            fails.push(format!("双拼: {m}"));
        }
    }
    let dt = t0.elapsed();
    assert!(
        fails.is_empty(),
        "{}/{} 路失败清单：\n{}",
        fails.len(),
        ws.len() * 2 - skip_full - skip_sp,
        fails.join("\n")
    );
    assert!(
        dt < Duration::from_secs(10),
        "{} 词 × 双路径总耗时 {dt:?}，超过 10s 护栏（查询路径性能劣化）",
        ws.len()
    );
    eprintln!(
        "常用词覆盖：全拼 {}/{}（缺陷跳过 {skip_full}）、双拼 {}/{}（缺陷跳过 {skip_sp}），总耗时 {dt:?}",
        ws.len() - skip_full,
        ws.len(),
        ws.len() - skip_sp,
        ws.len()
    );
}

/// 缺陷锁：确认两处 bug 还在。**此测试变红 = 引擎已修好**，届时把词放回主路径断言
/// 并删除本测试与 KNOWN_* 常量。
#[test]
fn known_bugs_are_still_locked() {
    let mut e = engine("bugfull", None);
    esc(&mut e);
    type_all(&mut e, "dangao");
    assert!(
        !e.candidates().iter().any(|c| c.text == KNOWN_FULL_BUG),
        "蛋糕全拼可达了！引擎已支持多切分查询 → 删除 KNOWN_FULL_BUG 跳过，恢复主路径断言"
    );
    let enc = ziranma_map();
    assert!(
        !enc.contains_key("ya"),
        "ziranma 补上 ya 码了！→ 删除 KNOWN_SP_BUG 跳过，恢复 澳大利亚 双拼断言"
    );
}

/// ≥4 音节的长词：只喂前 2 音节，词必须出现在补全候选的任意层。
#[test]
fn long_words_reachable_by_prefix_completion() {
    let mut e = engine("prefix", None);
    let mut fails = Vec::new();
    for (w, syls) in words() {
        if syls.len() < 4 {
            continue;
        }
        let keys = syls[..2].concat();
        esc(&mut e);
        type_all(&mut e, &keys);
        if !e.candidates().iter().any(|c| c.text == w) {
            let got: Vec<&str> = e
                .candidates()
                .iter()
                .take(PAGE)
                .map(|c| c.text.as_str())
                .collect();
            fails.push(format!("{w} [{keys}] 前 {PAGE} 候选 {got:?}"));
        }
    }
    assert!(fails.is_empty(), "前缀补全不可达：\n{}", fails.join("\n"));
}

/// 分词锁：拼音连写经 segment 必须能切回原音节序列（2+3/5 字词场景不靠候选排序）。
#[test]
fn pinyin_concat_segments_to_expected_syllables() {
    for (w, syls) in words() {
        let joined = syls.concat();
        let readings = segment(&joined);
        assert!(
            readings.iter().any(|r| *r == syls),
            "{w}: segment({joined:?}) 的 {} 种切分里没有 {syls:?}",
            readings.len()
        );
    }
}
