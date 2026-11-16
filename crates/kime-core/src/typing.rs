//! 打字语料回放：把一段真实文本喂进引擎，逐词统计「候选里能不能找到、排第几」。
//!
//! # 为什么要它
//!
//! 候选顺序的契约测试用的是单词/短串 fixture，而用户真正抱怨的是「打一整段话的时候
//! 顺序不对」。两者之间的差距是**上下文**：组句候选、pending 释放、学习提频都在连续
//! 输入中才生效。这个模块把一段文本当成一次连续输入来跑，产出可持久化的报告，让
//! 「改前跑一次、改后跑一次」变成可对照的数字，而不是感觉。
//!
//! # 指标口径
//!
//! - `top1`：该词出现在候选首位（用户不用翻页就能选到）。
//! - `top10`：该词出现在前十（一屏可见，按默认 page_size=10）。
//! - `miss`：候选里完全没有（用户只能重打或切模式）。
//!
//! 指标按**词**统计而不是按字符：用户是按词确认的，字符级统计会把「一个词打出来」
//! 记成多个样本，掩盖真实体验。
//!
//! # 边界（刻意不做）
//!
//! - 不自动选择期望词：引擎的选词索引是私有 API，模拟器只验证「候选可达」，
//!   然后提交当前首候选来推进状态。这样测的是候选顺序，而不是选词逻辑。
//! - 不做英文模式切换：英文词在中文模式下也必须能从候选里选到，这比「切英文直通」
//!   更难，也更接近用户抱怨的场景。

use crate::config::Config;
use crate::dict::Dict;
use crate::model::Lang;
use crate::Engine;

/// 一个被模拟的词。
#[derive(Debug, Clone)]
pub struct WordCase {
    /// 期望上屏的词
    pub text: String,
    /// 语言（决定用拼音还是字母喂键）
    pub lang: Lang,
    /// 实际喂进去的键序列
    pub keys: String,
    /// 该词在候选中的位置（1 起）；候选里没有则 None
    pub rank: Option<usize>,
    /// 候选总数
    pub cands: usize,
}

impl WordCase {
    pub fn top1(&self) -> bool {
        self.rank == Some(1)
    }
    pub fn top10(&self) -> bool {
        matches!(self.rank, Some(r) if r <= 10)
    }
    pub fn hit(&self) -> bool {
        self.rank.is_some()
    }
}

/// 一份回放报告。
#[derive(Debug, Clone, Default)]
pub struct TypingReport {
    pub cases: Vec<WordCase>,
}

impl TypingReport {
    pub fn total(&self) -> usize {
        self.cases.len()
    }
    pub fn top1(&self) -> usize {
        self.cases.iter().filter(|c| c.top1()).count()
    }
    pub fn top10(&self) -> usize {
        self.cases.iter().filter(|c| c.top10()).count()
    }
    pub fn miss(&self) -> usize {
        self.cases.iter().filter(|c| !c.hit()).count()
    }
    /// 紧凑摘要（一行）。
    pub fn summary(&self) -> String {
        format!(
            "词数={} top1={} top10={} miss={}",
            self.total(),
            self.top1(),
            self.top10(),
            self.miss()
        )
    }
}

/// 把文本切成「连续汉字段」与「连续 ASCII 词」。
///
/// 只认这两类：数字与标点当分隔符。混合文本因此被拆成若干段，每段单独回放。
pub fn split_runs(text: &str) -> Vec<(Lang, String)> {
    let mut out: Vec<(Lang, String)> = Vec::new();
    // 当前段：`(语言, 文本)`。用「有值/无值」代替 Option + expect——
    // 仓库 dylint 的 expect-in-non-result 禁止在返回非 Result 的函数里 expect。
    let mut cur: Option<(Lang, String)> = None;
    for ch in text.chars() {
        let lang = if is_cjk(ch) {
            Some(Lang::Zh)
        } else if ch.is_ascii_alphabetic() {
            Some(Lang::En)
        } else {
            None
        };
        match (lang, cur.as_ref().map(|(l, _)| *l)) {
            (Some(l), Some(c)) if l == c => {
                if let Some((_, buf)) = cur.as_mut() {
                    buf.push(ch);
                }
            }
            (Some(l), _) => {
                if let Some(prev) = cur.take() {
                    if !prev.1.is_empty() {
                        out.push(prev);
                    }
                }
                cur = Some((l, ch.to_string()));
            }
            (None, _) => {
                if let Some(prev) = cur.take() {
                    if !prev.1.is_empty() {
                        out.push(prev);
                    }
                }
            }
        }
    }
    if let Some(last) = cur {
        if !last.1.is_empty() {
            out.push(last);
        }
    }
    out
}

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
}

/// 中文段切成词：优先用词典最大频率分词（与上屏分词同口径），切不出来时退回单字。
fn zh_words(dict: &Dict, run: &str) -> Vec<String> {
    let segmented = crate::lm::segment_output_text(dict.conn(), run).unwrap_or_default();
    let words: Vec<String> = segmented
        .into_iter()
        .next()
        .unwrap_or_default()
        .into_iter()
        .filter(|w| !w.is_empty())
        .collect();
    if !words.is_empty() {
        return words;
    }
    run.chars().map(|c| c.to_string()).collect()
}

/// 词的读音：整词命中词典则用词典读音，否则逐字取最高频读音拼起来。
fn keys_for(dict: &Dict, word: &str, lang: Lang) -> String {
    match lang {
        Lang::En => word.to_ascii_lowercase(),
        Lang::Zh => {
            if let Ok(v) = dict.readings_of_text(word, 1) {
                if let Some((pinyin, _)) = v.into_iter().next() {
                    return pinyin.replace('\'', "");
                }
            }
            let mut out = String::new();
            for ch in word.chars() {
                let s = ch.to_string();
                if let Ok(v) = dict.readings_of_text(&s, 1) {
                    if let Some((pinyin, _)) = v.into_iter().next() {
                        out.push_str(&pinyin.replace('\'', ""));
                    }
                }
            }
            out
        }
    }
}

fn feed(engine: &mut Engine, keys: &str) -> Vec<String> {
    let mut last = Vec::new();
    for c in keys.chars() {
        engine.key(crate::Key {
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

fn escape(engine: &mut Engine) {
    for _ in 0..8 {
        if engine.preedit().is_empty() && engine.candidates().is_empty() {
            break;
        }
        engine.key(crate::Key {
            ch: None,
            code: crate::engine::KEY_ESC,
            shift: false,
            ctrl: false,
            alt: false,
        });
    }
}

fn commit_top(engine: &mut Engine) {
    if engine.candidates().is_empty() {
        return;
    }
    engine.key(crate::Key {
        ch: None,
        code: crate::engine::KEY_SPACE,
        shift: false,
        ctrl: false,
        alt: false,
    });
}

/// 回放一段文本：消费 `dict` 建引擎，逐词统计，返回报告。
///
/// 统计口径见模块文档。英文词用字母键喂（中文模式下英文候选也必须可达）；
/// 中文词用词典读音喂。每段之后提交首候选或清空组合，保证下一段从干净状态开始。
pub fn run(dict: Dict, config: Config, text: &str) -> TypingReport {
    let runs = split_runs(text);
    // 先解析全部读音：Dict 在构造 Engine 时会被移动，之后无法再查。
    let mut plan: Vec<(Lang, String, String)> = Vec::new();
    for (lang, run) in &runs {
        match lang {
            Lang::Zh => {
                for w in zh_words(&dict, run) {
                    let keys = keys_for(&dict, &w, Lang::Zh);
                    if keys.is_empty() {
                        continue;
                    }
                    plan.push((Lang::Zh, w, keys));
                }
            }
            Lang::En => {
                for w in run.split_whitespace() {
                    plan.push((Lang::En, w.to_string(), w.to_ascii_lowercase()));
                }
            }
        }
    }

    let mut engine = Engine::new(dict, config);
    let mut cases = Vec::with_capacity(plan.len());
    for (lang, text, keys) in plan {
        escape(&mut engine);
        let cands = feed(&mut engine, &keys);
        let rank = cands.iter().position(|c| c == &text).map(|i| i + 1);
        cases.push(WordCase {
            text,
            lang,
            keys,
            rank,
            cands: cands.len(),
        });
        // 推进状态：中文词提交首候选（模拟用户直接选第一个），英文词清空组合。
        match lang {
            Lang::Zh => commit_top(&mut engine),
            Lang::En => escape(&mut engine),
        }
    }
    TypingReport { cases }
}

/// 把报告渲染成 markdown 表格（可持久化、可 diff）。
pub fn render_markdown(title: &str, report: &TypingReport, limit: usize) -> String {
    let mut s = String::new();
    s.push_str(&format!("# {title}\n\n"));
    s.push_str(&format!(
        "总计 {} 词：top1 {} / top10 {} / miss {}\n\n",
        report.total(),
        report.top1(),
        report.top10(),
        report.miss()
    ));
    s.push_str("| # | 词 | 语言 | 键 | 名次 | 候选数 |\n|---|---|---|---|---|---|\n");
    // 单元格文本必须转义：候选里可能含 `|`（短语导入不做字符过滤）或换行，
    // 原样写进 Markdown 表格会把列数撑乱、报告失去可读性。
    let cell = |v: &str| v.replace('|', "\\|").replace(['\n', '\r'], " ");
    for (i, c) in report.cases.iter().take(limit).enumerate() {
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            i + 1,
            cell(&c.text),
            match c.lang {
                Lang::Zh => "zh",
                Lang::En => "en",
            },
            cell(&c.keys),
            c.rank.map(|r| r.to_string()).unwrap_or_else(|| "-".into()),
            c.cands
        ));
    }
    if report.cases.len() > limit {
        s.push_str(&format!(
            "\n（仅列前 {limit} 条，共 {} 条）\n",
            report.cases.len()
        ));
    }
    s
}
