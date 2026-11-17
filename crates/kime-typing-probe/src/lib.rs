//! `kime-typing-probe` — 离线 mock 敲字体验评测 crate。
//!
//! 仅向下依赖 `kime-core` 与标准库（+`serde` 用于报告序列化），不依赖
//! platform/render，不引入 `reqwest`。LLM 仅在显式配置 `KIME_LLM_ENDPOINT`
//! 时可选，缺省走规则建议，CI 完全离线。
//!
//! 设计目标：读取 TSV 语料、**一个引擎连续回放整份语料**、采集命中率/候选位置/翻页/
//! 回删/延迟/上下文命中，输出 rules-only 或 LLM 可消费的 JSON/Markdown 报告。
//!
//! 与 kime-core 排序层契约：中文精确 → 英文精确 → 中文补全 → 英文补全，上下文只层内重排。
//! 本 crate 不修改 core 任何排序逻辑，只消费公共 API（`Engine`、`Key`、`Outcome`、`Candidate`）。

pub mod corpus;
pub mod llm;
pub mod metrics;
pub mod report;
pub mod runner;

/// 评测入口：一次运行整个语料文件，返回可序列化的 [`ProbeResult`]。
///
/// 这是 crate 对外最小公共 API。`run` 内部：
/// 1. 通过 [`corpus::Corpus::load_tsv`] 加载语料；
/// 2. **只建一个引擎**（[`runner::Runner::with_config`]），按语料顺序逐条回放，
///    条目之间只清组合缓冲、不重建引擎；
/// 3. 通过 [`metrics::SentenceMetric`] 汇总每条与全局指标；
/// 4. 返回 [`ProbeResult`]，可交给 [`report::ProbeReport`] 渲染 JSON/Markdown。
///
/// 为什么必须共用一个引擎：kime 的实时上下文排序（`lm_prev_text` /
/// `context_tail` / 实时 bigram）是跨条目累积的。每条新建引擎会让上下文加成恒为
/// 零，测不出「前一个词影响下一个词」的体验差异——那正是这次排序改动要验的东西。
///
/// `config` 由调用方给（CLI 传 `Config::load()` 并按 `--mode` 覆盖 `shuangpin`）。
/// **语料里的键必须与 `--mode` 同一方案**：语料记的是「按哪种方案敲出来的键」，
/// 全拼音料喂给双拼引擎会被逐键解码成别的音节。
///
/// # Errors
/// 语料读取失败、`Dict::open` 失败或按键回放失败时返回 `anyhow::Error`。
pub fn run(
    corpus_path: &std::path::Path,
    dict_path: &std::path::Path,
    config: kime_core::config::Config,
) -> anyhow::Result<ProbeResult> {
    let corpus = corpus::Corpus::load_tsv(corpus_path)?;
    let mut runner = runner::Runner::with_config(dict_path, config)?;
    let mut sentences = Vec::with_capacity(corpus.entries.len());
    for entry in &corpus.entries {
        // 先清上一条的残留组合，再 begin：begin 会把引擎当前页记成基准，
        // 顺序反了会让第一条多记一次假翻页。
        runner.reset_input();
        runner.begin(entry);
        let lang = runner.lang();
        let events = key_events_for(entry, lang);
        runner.run(&events)?;
        sentences.push(metrics::SentenceMetric::from_session(
            &entry.id,
            runner.trace(),
        ));
    }
    let summary = metrics::Summary::from_sentences(&sentences);
    let per_lang = metrics::PerLang::from_sentences(&sentences);
    Ok(ProbeResult {
        schema_version: crate::report::SCHEMA_VERSION.to_string(),
        generated_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        corpus_id: corpus_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string(),
        total_sentences: sentences.len(),
        summary,
        per_lang,
        per_sentence: sentences,
    })
}

/// 把一条语料展开成按键序列。
///
/// - 中文：`pinyin` 以 `'` 分隔音节，这里丢掉分隔符只留字母（引擎按整串拼音解码）。
/// - 英文：`pinyin` 为空，退化为逐字符输入 `text` 的小写形式。
/// - **空白是显式的上屏边界**：语料里 `wo'men jin'tian` 会展开成
///   `w o m e n Space j i n t i a n Space`。这样一份语料能表达「打完一个词按空格
///   上屏、再打下一个」，上下文才会在词与词之间累积。
///
/// 非字母字符（`'` 音节分隔符、标点、数字）跳过：引擎的组合缓冲只收 a-z。
/// 末尾若最后一个事件不是 `Space`，补一个，保证组合被提交而不是悬着。
fn key_events_for(entry: &corpus::CorpusEntry, lang: runner::Lang) -> Vec<runner::KeyEvent> {
    let source = match lang {
        runner::Lang::Zh if !entry.pinyin.is_empty() => entry.pinyin.as_str(),
        _ => entry.text.as_str(),
    };
    let mut events: Vec<runner::KeyEvent> = Vec::with_capacity(source.len() + 1);
    let mut last_was_space = false;
    for c in source.chars() {
        if c.is_ascii_alphabetic() {
            events.push(runner::KeyEvent::Char {
                ch: c.to_ascii_lowercase(),
            });
            last_was_space = false;
        } else if c.is_whitespace() {
            // 连续空白只算一次边界，避免 `a  b` 产生两个 Space。
            if !last_was_space {
                events.push(runner::KeyEvent::Space);
                last_was_space = true;
            }
        }
    }
    if !last_was_space {
        events.push(runner::KeyEvent::Space);
    }
    events
}

/// 整个评测的顶层结果：与 [`report::ProbeReport`] 同结构，可直接序列化为 JSON。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ProbeResult {
    pub schema_version: String,
    pub generated_at: u64,
    pub corpus_id: String,
    pub total_sentences: usize,
    pub summary: metrics::Summary,
    pub per_lang: metrics::PerLang,
    pub per_sentence: Vec<metrics::SentenceMetric>,
}

impl ProbeResult {
    /// 渲染为 Markdown 文本（等价于 [`report::render_markdown`]）。
    pub fn to_markdown(&self) -> String {
        report::render_markdown(self)
    }

    /// 序列化为紧凑 JSON（等价于 [`report::to_json`]）。
    pub fn to_json(&self) -> String {
        report::to_json(self)
    }
}
