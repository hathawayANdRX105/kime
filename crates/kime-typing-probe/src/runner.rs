//! 按键回放 runner：在 sqlite 词库上构造 `kime_core::Engine`，逐键回放 mock 按键脚本，
//! 采集每键 `Outcome`、preedit、candidates、page、耗时。
//!
//! 生产 runner 接受已存在的 `dict_path`；测试侧由 `tempfile` 提供临时 sqlite 路径
//!（仅 dev-dep，随 crate 销毁即清）。不引入 platform/render，不依赖网络。
//! kime-core 的按键常量（`KEY_*`）已 pub，此处直接复用，避免常量 drift 与 orphan-rule 违规。

use crate::corpus::CorpusEntry;
use kime_core::{
    dict::Dict,
    engine::{KEY_BACKSPACE, KEY_EQUAL, KEY_ESC, KEY_MINUS, KEY_SPACE},
    Engine, Key, Outcome,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Instant;

/// 单次按键事件：`run()` 按语料读音生成，测试可手工构造具体序列。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum KeyEvent {
    /// 可打印字符（逐字输入拼音/英文字母）
    Char { ch: char },
    /// 退格（KEY_BACKSPACE，组合内删字或弹词）
    Backspace,
    /// 空格（KEY_SPACE，选词/提交/释放 pending）
    Space,
    /// 翻页上一段（KEY_MINUS）
    PageUp,
    /// 翻页下一段（KEY_EQUAL）
    PageDown,
    /// 数字选词（0-9，evdev keycode 2..=11）
    SelectDigit { digit: u32 },
    /// 清组合/取消（KEY_ESC）
    Esc,
}

/// 单键采样记录：每次 `Engine::key` 返回后记录的瞬时状态。
///
/// 不存 `Outcome`：`kime_core::Outcome` 未派生 `Serialize`，而这里只需要
/// 「是否上屏、上屏了什么」——`committed` 已承载，`Outcome` 的其余变体
/// （Consumed/Ignored）对报告无信息量。
#[derive(Clone, Debug, Serialize)]
pub struct KeySample {
    /// 输入的按键事件
    pub event: KeyEvent,
    /// 耗时（毫秒）
    pub latency_ms: f64,
    /// 回放后的 preedit 显示串
    pub preedit: String,
    /// 回放后的全部候选（快照，已 clone）
    pub candidates: Vec<String>,
    /// (当前页, 页大小)
    pub page: (usize, usize),
    /// 回放后是否发生 Commit（上屏文本）
    pub committed: Option<String>,
}

/// 一次会话的完整回放轨迹：句级 key samples 与汇总计数。
#[derive(Clone, Debug, Serialize)]
pub struct SessionTrace {
    /// 句子文本（目标）
    pub text: String,
    /// 句子读音（拼音，中文；英文为空）
    pub pinyin: String,
    /// 语言桶：zh / en
    pub lang: Lang,
    /// 逐键采样
    pub samples: Vec<KeySample>,
    /// 该句实际上屏序列（Commit 拼接）
    pub committed: String,
    /// 每键耗时集合（毫秒）
    pub latencies_ms: Vec<f64>,
    /// 页面翻转次数（以 page_index 变化为准，Runner 是唯一计数来源）
    pub page_turns: usize,
    /// 同一句内上屏不同文本的次数（重选）
    pub reselections: usize,
}

/// 语言标记：由语料条目的读音是否为空推断，与 [`crate::corpus::Corpus::zh_entries`] 同一判据。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lang {
    #[default]
    Zh,
    En,
}

impl Lang {
    /// 由条目推断语言：读音非空 = 中文。
    ///
    /// 曾经按 id/文件名里的 `zh` 子串判断，而真实 id 形如
    /// `path/zh_articles.tsv:12`——只认 `starts_with("zh")` 时全部落进 `En`，
    /// per-lang 统计随之失真。判据统一到 [`crate::corpus::CorpusEntry::is_zh`]。
    pub fn of_entry(entry: &CorpusEntry) -> Self {
        if entry.is_zh() {
            Lang::Zh
        } else {
            Lang::En
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Lang::Zh => "zh",
            Lang::En => "en",
        }
    }
}

/// 将 `KeyEvent` 转换为 `kime_core::Key`，供 `Engine::key` 消费。
fn to_key(event: &KeyEvent) -> Key {
    match event {
        KeyEvent::Char { ch } => Key {
            ch: Some(*ch),
            code: 0,
            shift: false,
            ctrl: false,
            alt: false,
        },
        KeyEvent::Backspace => Key {
            ch: None,
            code: KEY_BACKSPACE,
            shift: false,
            ctrl: false,
            alt: false,
        },
        KeyEvent::Space => Key {
            ch: None,
            code: KEY_SPACE,
            shift: false,
            ctrl: false,
            alt: false,
        },
        KeyEvent::PageUp => Key {
            ch: None,
            code: KEY_MINUS,
            shift: false,
            ctrl: false,
            alt: false,
        },
        KeyEvent::PageDown => Key {
            ch: None,
            code: KEY_EQUAL,
            shift: false,
            ctrl: false,
            alt: false,
        },
        KeyEvent::SelectDigit { digit } => Key {
            ch: None,
            // evdev 0-9 keycode 为 2..=11，'0' 对应 2
            code: 2 + *digit,
            shift: false,
            ctrl: false,
            alt: false,
        },
        KeyEvent::Esc => Key {
            ch: None,
            code: KEY_ESC,
            shift: false,
            ctrl: false,
            alt: false,
        },
    }
}

/// Runner：按键回放器。持有一个 Engine 与累积的 SessionTrace。
pub struct Runner {
    engine: Engine,
    lang: Lang,
    target_text: String,
    trace: SessionTrace,
    /// 已提交文本片段（Commit 拼接，用于命中判断）
    committed_parts: Vec<String>,
    /// 同一句内对同一读音的不同候选被 Commit 的次数（重选计数）
    reselect_count: usize,
    /// 页面翻转计数（以 page_index 变化为准）
    page_turns: usize,
    last_page_index: usize,
}

impl Runner {
    /// 构造 runner：用已存在的 sqlite dict 路径 + 单条语料。
    ///
    /// 默认配置（关闭纠错/双拼）：`Config { shuangpin: None, correction: false,
    /// ..Default::default() }`，`dict_path` 指向 tempfile 或已有 sqlite 库，不依赖
    /// 用户 ~/.config/kime。
    pub fn new(dict_path: &Path, entry: &CorpusEntry) -> anyhow::Result<Self> {
        let dict = Dict::open(dict_path).map_err(|e| anyhow::anyhow!("dict open: {e}"))?;
        let config = kime_core::config::Config {
            shuangpin: None,
            correction: false,
            ..Default::default()
        };
        let engine = Engine::new(dict, config);
        let lang = Lang::of_entry(entry);
        Ok(Self {
            engine,
            lang,
            target_text: entry.text.clone(),
            trace: SessionTrace {
                text: entry.text.clone(),
                pinyin: entry.pinyin.clone(),
                lang,
                samples: Vec::new(),
                committed: String::new(),
                latencies_ms: Vec::new(),
                page_turns: 0,
                reselections: 0,
            },
            committed_parts: Vec::new(),
            reselect_count: 0,
            page_turns: 0,
            last_page_index: 0,
        })
    }

    /// 逐键回放 `events`，返回 `SessionTrace`（采样后可交给 `metrics` 汇总）。
    pub fn run(&mut self, events: &[KeyEvent]) -> anyhow::Result<()> {
        for event in events {
            self.apply_one(event)?;
        }
        self.trace.committed = self.committed_parts.concat();
        self.trace.page_turns = self.page_turns;
        self.trace.reselections = self.reselect_count;
        Ok(())
    }

    fn apply_one(&mut self, event: &KeyEvent) -> anyhow::Result<()> {
        let key = to_key(event);
        let start = Instant::now();
        let outcome = self.engine.key(key);
        let latency_ms = start.elapsed().as_secs_f64() * 1000.0;

        let preedit = self.engine.preedit().to_string();
        let candidates: Vec<String> = self
            .engine
            .candidates()
            .iter()
            .map(|c| c.text.clone())
            .collect();
        let page = self.engine.page();

        // 翻页计数：page_index 变化即一次翻页
        if page.0 != self.last_page_index {
            self.page_turns += 1;
        }
        self.last_page_index = page.0;

        let committed = match &outcome {
            Outcome::Commit(text) => {
                self.committed_parts.push(text.clone());
                // 重选：同一句内上屏不同文本（同一组合内不同词）判为重选
                if self.committed_parts.len() > 1 {
                    let prev = self.committed_parts[self.committed_parts.len() - 2].as_str();
                    if prev != text {
                        self.reselect_count += 1;
                    }
                }
                Some(text.clone())
            }
            _ => None,
        };

        let sample = KeySample {
            event: event.clone(),
            latency_ms,
            preedit,
            candidates,
            page,
            committed,
        };
        self.trace.latencies_ms.push(latency_ms);
        self.trace.samples.push(sample);
        Ok(())
    }

    /// 页面翻转总次数（以 `page_index` 变化为准）。
    pub fn page_turns(&self) -> usize {
        self.page_turns
    }

    /// 重选总次数（同一句内上屏不同文本）。
    pub fn reselect_count(&self) -> usize {
        self.reselect_count
    }

    pub fn trace(&self) -> &SessionTrace {
        &self.trace
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn lang(&self) -> Lang {
        self.lang
    }

    pub fn target_text(&self) -> &str {
        &self.target_text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_follows_entry_reading() {
        let zh = CorpusEntry {
            id: "path/zh_articles.tsv:12".to_string(),
            freq: 1,
            text: "我们".to_string(),
            pinyin: "wo'men".to_string(),
        };
        let en = CorpusEntry {
            id: "path/en_articles.tsv:3".to_string(),
            freq: 1,
            text: "hello".to_string(),
            pinyin: String::new(),
        };
        assert_eq!(Lang::of_entry(&zh), Lang::Zh);
        assert_eq!(Lang::of_entry(&en), Lang::En);
    }
}
