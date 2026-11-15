//! 调试语料回放：把 `tests/fixtures/debug_corpus.tsv` 当成候选排序的可执行契约。
//!
//! # 为什么要这份语料
//!
//! 候选顺序是本仓最容易被改坏的契约：它同时受语言、层、频率、用户学习、上下文加成
//! 五个因素影响，而每个因素都有单独的测试文件。排查时如果每次都要手写 fixture、
//! 建临时库、拼按键序列，复现成本高到没人愿意做——于是「本地跑一下看看」变成
//! 空话，问题只能靠 CI 撞出来。
//!
//! 这份语料把复现步骤压成一行数据：加用例 = 追加一行，跑用例 = 一条命令。
//! 断言语法见语料文件头部注释。
//!
//! # 与 debug CLI 的关系
//!
//! 同一个文件也能被 `kime debug --dict <库> --corpus <文件>` 回放（用真实词库），
//! 供人工/agent 在改代码前后各跑一次做对照。测试与 CLI 共用格式，因此「测试里
//! 绿的语料」与「本地跑通的语料」是同一份。

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::{Engine, Key};
use kime_shuangpin::Scheme;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const CORPUS: &str = include_str!("fixtures/debug_corpus.tsv");
const CHINESE: &str = include_str!("fixtures/common_words.tsv");
const ENGLISH: &str = include_str!("fixtures/debug_english.dict.yaml");

/// 一条语料：输入模式 + 按键 + 断言。
#[derive(Debug)]
struct Case {
    mode: Mode,
    keys: String,
    expect: Vec<Expect>,
    line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    FullPinyin,
    Shuangpin(Scheme),
}

#[derive(Debug)]
enum Expect {
    /// 第一个候选必须是它
    First(String),
    /// 候选中必须出现
    Has(String),
    /// 候选中不得出现
    Absent(String),
    /// 左必须先于右
    Before(String, String),
}

impl Expect {
    fn parse(raw: &str) -> Result<Self, String> {
        if let Some(v) = raw.strip_prefix("first=") {
            return Ok(Expect::First(v.to_string()));
        }
        if let Some(v) = raw.strip_prefix("has=") {
            return Ok(Expect::Has(v.to_string()));
        }
        if let Some(v) = raw.strip_prefix("absent=") {
            return Ok(Expect::Absent(v.to_string()));
        }
        match raw.split_once('<') {
            Some((l, r)) if !l.is_empty() && !r.is_empty() => {
                Ok(Expect::Before(l.to_string(), r.to_string()))
            }
            _ => Err(format!(
                "无法解析断言 {raw:?}（可用 first= / has= / absent= / A<B）"
            )),
        }
    }
}

fn parse_mode(raw: &str, line: usize) -> Result<Mode, String> {
    match raw {
        "full" => Ok(Mode::FullPinyin),
        other => match other.strip_prefix("shuangpin:") {
            // 方案名显式列出：新增方案要同时改这里与码表，语料里写错名字必须报错，
            // 不能静默退化成某个默认方案。
            Some("ziranma") => Ok(Mode::Shuangpin(Scheme::Ziranma)),
            Some("xiaohe") => Ok(Mode::Shuangpin(Scheme::Xiaohe)),
            Some(name) => Err(format!("第 {line} 行：未知双拼方案 {name:?}")),
            None => Err(format!("第 {line} 行：未知模式 {raw:?}")),
        },
    }
}

fn parse_corpus(text: &str) -> Result<Vec<Case>, String> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let row = raw.trim();
        if row.is_empty() || row.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = row.split('\t').collect();
        if cols.len() != 3 {
            return Err(format!(
                "第 {line} 行必须正好 3 列（mode/keys/expect），实际 {cols:?}"
            ));
        }
        let expect = cols[2]
            .split(';')
            .filter(|s| !s.trim().is_empty())
            .map(Expect::parse)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("第 {line} 行：{e}"))?;
        out.push(Case {
            mode: parse_mode(cols[0], line)?,
            keys: cols[1].to_string(),
            expect,
            line,
        });
    }
    Ok(out)
}

/// 中文词库：常用词 fixture，词频按行号递减（库内排名无平票）。
fn chinese_yaml() -> String {
    let mut s = String::from("---\nname: debug-corpus-cn\n...\n");
    for (i, line) in CHINESE.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let (w, p) = line.split_once('\t').expect("fixture 行必须是 词\\t拼音");
        s.push_str(&format!(
            "{w}\t{}\t{}\n",
            p.replace('\'', " "),
            10_000_000i64 - i as i64 * 1000
        ));
    }
    s
}

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kime_debug_corpus_{tag}_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// 建库：中文常用词 + 英文调试词。每例一个引擎（组合状态互不污染）。
fn engine(tag: &str, mode: Mode) -> (Engine, PathBuf) {
    let dir = tmp_dir(tag);
    let db = dir.join("dict.sqlite3");
    let mut dict = Dict::open(&db).unwrap();
    let cn = dir.join("cn.yaml");
    fs::write(&cn, chinese_yaml()).unwrap();
    dict.import(&cn).unwrap();
    let en = dir.join("en.yaml");
    fs::write(&en, ENGLISH).unwrap();
    dict.import_english(&en).unwrap();
    let engine = Engine::new(
        dict,
        Config {
            shuangpin: match mode {
                Mode::FullPinyin => None,
                Mode::Shuangpin(s) => Some(s),
            },
            ..Config::default()
        },
    );
    (engine, dir)
}

fn feed(engine: &mut Engine, keys: &str) -> Vec<String> {
    let mut last = Vec::new();
    for c in keys.chars() {
        engine.key(Key {
            ch: Some(c),
            code: 0,
            shift: false,
            ctrl: false,
            alt: false,
        });
        last = engine.candidates().iter().map(|c| c.text.clone()).collect();
    }
    last
}

fn position(cands: &[String], text: &str) -> Option<usize> {
    cands.iter().position(|c| c == text)
}

fn check(case: &Case, cands: &[String]) -> Result<(), String> {
    for e in &case.expect {
        match e {
            Expect::First(t) => {
                if cands.first().map(String::as_str) != Some(t.as_str()) {
                    return Err(format!("首候选应为 {t:?}，实际 {:?}", cands.first()));
                }
            }
            Expect::Has(t) => {
                if position(cands, t).is_none() {
                    return Err(format!("候选里缺少 {t:?}"));
                }
            }
            Expect::Absent(t) => {
                if position(cands, t).is_some() {
                    return Err(format!("候选里不该出现 {t:?}"));
                }
            }
            Expect::Before(l, r) => match (position(cands, l), position(cands, r)) {
                (Some(a), Some(b)) if a < b => {}
                (a, b) => {
                    return Err(format!(
                        "{l:?} 必须在 {r:?} 之前，实际位置 左={a:?} 右={b:?}"
                    ))
                }
            },
        }
    }
    Ok(())
}

#[test]
fn debug_corpus_cases_hold() {
    let cases = parse_corpus(CORPUS).expect("语料必须能解析");
    assert!(!cases.is_empty(), "语料不得为空");

    let mut failures: Vec<String> = Vec::new();
    for case in &cases {
        let (mut engine, dir) = engine(&format!("line{}", case.line), case.mode);
        let cands = feed(&mut engine, &case.keys);
        if let Err(e) = check(case, &cands) {
            // 失败信息带上实际候选表：语料的作用就是「复现 + 看清顺序」，
            // 只说「必须在…之前」而不给实际列表，排查时还得再跑一次 CLI。
            let shown: Vec<&str> = cands.iter().take(24).map(String::as_str).collect();
            failures.push(format!(
                "第 {} 行 mode={:?} keys={:?}: {e}\n          实际候选[{}] = {:?}",
                case.line,
                case.mode,
                case.keys,
                cands.len(),
                shown
            ));
        }
        drop(engine);
        let _ = fs::remove_dir_all(dir);
    }

    assert!(
        failures.is_empty(),
        "调试语料有 {} 条不符合契约：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn corpus_parser_rejects_malformed_rows() {
    // 解析器必须是「坏数据报错」而不是「静默跳过」：语料是契约，静默跳过等于
    // 少跑一条用例却没人知道。
    assert!(parse_corpus("full\tnihao").is_err(), "缺列必须报错");
    assert!(
        parse_corpus("bogus\tnihao\tfirst=你好").is_err(),
        "未知模式必须报错"
    );
    assert!(
        parse_corpus("full\tnihao\tnonsense").is_err(),
        "无法解析的断言必须报错"
    );
    assert!(
        parse_corpus("shuangpin:nope\tnihao\tfirst=你好").is_err(),
        "未知双拼方案必须报错"
    );
    assert!(
        parse_corpus("# 注释\n\nfull\tnihao\tfirst=你好\n").is_ok(),
        "注释与空行必须跳过"
    );
}

#[test]
fn corpus_file_path_is_stable_for_cli_reuse() {
    // 语料文件路径固定：CLI 的 --corpus 与测试引用同一份，agent 改一处即可。
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/debug_corpus.tsv");
    assert!(p.is_file(), "语料文件必须存在：{}", p.display());
}
