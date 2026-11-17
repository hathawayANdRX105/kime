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
#[derive(Clone, Debug, Default, Serialize)]
pub struct SessionTrace {
    /// 目标文本
    pub text: String,
    /// 读音（拼音，中文；英文为空）
    pub pinyin: String,
    /// 语言桶：zh / en
    pub lang: Lang,
    /// 逐键采样
    pub samples: Vec<KeySample>,
    /// 实际上屏序列（Commit 拼接）
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

/// `reset_input` 的 Esc 上限：足够清掉任何组合缓冲，又不会在引擎异常时死循环。
const MAX_RESET_KEYS: usize = 16;

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
    /// 构造 runner：用已存在的 sqlite dict 路径 + **调用方给的配置**，引擎可跨条目复用。
    ///
    /// 引擎必须复用才能让上下文参与：kime 的实时上下文排序（`lm_prev_text` /
    /// `context_tail` / bigram）是**跨条目累积**的，每条语料新建引擎等于每次从零开始，
    /// 上下文加成永远为空，测不出「前一个词影响下一个词」的体验差异。
    ///
    /// 配置由调用方给而不是写死：用户日常用双拼（Ziranma），把 `shuangpin` 写死成
    /// `None` 会让 `ui` 这类双拼键串解成别的音节，测的就不是用户实际敲的键。
    pub fn with_config(
        dict_path: &Path,
        config: kime_core::config::Config,
    ) -> anyhow::Result<Self> {
        let dict = Dict::open(dict_path).map_err(|e| anyhow::anyhow!("dict open: {e}"))?;
        let engine = Engine::new(dict, config);
        Ok(Self {
            engine,
            lang: Lang::Zh,
            target_text: String::new(),
            trace: SessionTrace::default(),
            committed_parts: Vec::new(),
            reselect_count: 0,
            page_turns: 0,
            last_page_index: 0,
        })
    }

    /// 单条语料的便捷构造：默认配置（关闭纠错/双拼）+ 立刻指向该条目。
    ///
    /// 单条测试用这个就够了；需要双拼或跨条目连续回放时用
    /// [`Runner::with_config`] + [`Runner::begin`]。
    pub fn new(dict_path: &Path, entry: &CorpusEntry) -> anyhow::Result<Self> {
        let config = kime_core::config::Config {
            shuangpin: None,
            correction: false,
            ..Default::default()
        };
        let mut runner = Self::with_config(dict_path, config)?;
        runner.begin(entry);
        Ok(runner)
    }

    /// 指向新的目标条目，重置句级采样；**引擎状态保留**（上下文/学习跨条目累积）。
    pub fn begin(&mut self, entry: &CorpusEntry) {
        self.lang = Lang::of_entry(entry);
        self.target_text = entry.text.clone();
        self.trace = SessionTrace {
            text: entry.text.clone(),
            pinyin: entry.pinyin.clone(),
            lang: self.lang,
            ..SessionTrace::default()
        };
        self.committed_parts.clear();
        self.reselect_count = 0;
        self.page_turns = 0;
        self.last_page_index = self.engine.page().0;
    }

    /// 清空组合缓冲：连续 Esc 直到 preedit 与候选都空。
    ///
    /// 在 [`Runner::begin`] **之前**调用，条目之间隔离彼此的残留组合，同时不污染
    /// 下一条的逐键采样（Esc 不计入它的 `samples`）。引擎的学习与上下文不受影响。
    pub fn reset_input(&mut self) {
        for _ in 0..MAX_RESET_KEYS {
            if self.engine.preedit().is_empty() && self.engine.candidates().is_empty() {
                return;
            }
            self.engine.key(to_key(&KeyEvent::Esc));
        }
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
