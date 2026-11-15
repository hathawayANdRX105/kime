//! 打字语料回放（确定性）：用 fixtures 词库跑 `tests/fixtures/typing/*.txt`。
//!
//! 与 `kime typing` 的分工：
//! - 本测试：**确定性**，跑在 fixtures 词库上，CI 里守「回放器不坏、报告可生成、
//!   候选可达性不为零」这几条结构性契约；
//! - `kime typing --dict <真实库>`：测**真实体验**（top1/top10/miss），结果落
//!   `reports/typing-*.md`，人读、可 diff。
//!
//! 两者共用 `typing::run`，因此「本地跑通的数字」与「CI 守的行为」不会分叉。

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::typing;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const ZH: &str = include_str!("fixtures/typing/zh_100.txt");
const EN: &str = include_str!("fixtures/typing/en_100.txt");
const MIXED: &str = include_str!("fixtures/typing/mixed.txt");
const CHINESE: &str = include_str!("fixtures/common_words.tsv");
const ZH_CHARS: &str = include_str!("fixtures/typing/zh_chars.tsv");
const ENGLISH: &str = include_str!("fixtures/debug_english.dict.yaml");

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kime_typing_{tag}_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// 与 debug 语料同一套 fixtures：常用词 + 调试英文词。
fn dict(tag: &str) -> (Dict, PathBuf) {
    let dir = tmp_dir(tag);
    let db = dir.join("dict.sqlite3");
    let mut d = Dict::open(&db).unwrap();
    let mut cn = String::from("---\nname: typing-cn\n...\n");
    for (i, line) in CHINESE.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let (w, p) = line.split_once('\t').unwrap();
        cn.push_str(&format!(
            "{w}\t{}\t{}\n",
            p.replace('\'', " "),
            10_000_000i64 - i as i64 * 1000
        ));
    }
    // 单字表：常用词表只有多字词，而中文语料按词典分词后会落到单字，
    // 缺单字会让 `keys_for` 解析不出读音、整条用例被跳过。
    // 单字的读音取自真实词库（fixtures/typing/zh_chars.tsv），因此这里不是编的。
    for (i, line) in ZH_CHARS
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
    {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 2 {
            continue;
        }
        cn.push_str(&format!(
            "{}\t{}\t{}\n",
            cols[0],
            cols[1],
            9_000_000i64 - i as i64 * 1000
        ));
    }
    let cny = dir.join("cn.yaml");
    fs::write(&cny, cn).unwrap();
    d.import(&cny).unwrap();
    let eny = dir.join("en.yaml");
    fs::write(&eny, ENGLISH).unwrap();
    d.import_english(&eny).unwrap();
    (d, dir)
}

fn run_case(tag: &str, text: &str) -> typing::TypingReport {
    let (d, dir) = dict(tag);
    let report = typing::run(d, Config::default(), text);
    let _ = fs::remove_dir_all(dir);
    report
}

#[test]
fn typing_corpus_runs_and_reports() {
    for (tag, text) in [("zh", ZH), ("en", EN), ("mixed", MIXED)] {
        let r = run_case(tag, text);
        assert!(!r.cases.is_empty(), "{tag} 语料必须产出用例");
        // 结构性契约：回放器必须能渲染报告，且每个用例都有确定的候选数。
        let md = typing::render_markdown(tag, &r, 1000);
        assert!(md.contains("| # | 词 |"), "{tag} 报告缺表头");
        assert!(
            r.cases.iter().all(|c| !c.keys.is_empty()),
            "{tag} 有用例没解析出键"
        );
        // fixtures 词库很小：中文常用词必须能命中（否则回放器接错了词库）。
        if tag == "zh" {
            assert!(r.miss() < r.total(), "中文语料在 fixtures 词库上 miss 过多");
        }
    }
}

#[test]
fn typing_split_runs_separates_scripts() {
    let runs = typing::split_runs("使用 kime 打字，输入 ru 会看到 rug。");
    let zh: Vec<&str> = runs
        .iter()
        .filter(|(l, _)| *l == kime_core::Lang::Zh)
        .map(|(_, t)| t.as_str())
        .collect();
    let en: Vec<&str> = runs
        .iter()
        .filter(|(l, _)| *l == kime_core::Lang::En)
        .map(|(_, t)| t.as_str())
        .collect();
    // 标点与空格是分隔符，因此「。」不构成一个 run。
    assert_eq!(zh, vec!["使用", "打字", "输入", "会看到"]);
    assert_eq!(en, vec!["kime", "ru", "rug"]);
}

#[test]
fn typing_report_renders_rank_and_summary() {
    let report = typing::TypingReport {
        cases: vec![
            typing::WordCase {
                text: "你好".into(),
                lang: kime_core::Lang::Zh,
                keys: "nihao".into(),
                rank: Some(1),
                cands: 40,
            },
            typing::WordCase {
                text: "missing".into(),
                lang: kime_core::Lang::En,
                keys: "missing".into(),
                rank: None,
                cands: 30,
            },
        ],
    };
    let md = typing::render_markdown("t", &report, 10);
    assert!(md.contains("top1 1 / top10 1 / miss 1"), "摘要算错：{md}");
    assert!(md.contains("| 你好 | zh | nihao | 1 | 40 |"));
    assert!(md.contains("| missing | en | missing | - | 30 |"));
}

#[test]
fn typing_corpus_files_are_present() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typing");
    for f in ["zh_100.txt", "en_100.txt", "mixed.txt"] {
        assert!(dir.join(f).is_file(), "缺语料文件 {f}");
    }
}
