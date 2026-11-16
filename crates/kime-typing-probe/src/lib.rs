//! `kime-typing-probe` — 离线 mock 敲字体验评测 crate。
//!
//! 仅向下依赖 `kime-core` 与标准库（+`serde` 用于报告序列化），不依赖
//! platform/render，不引入 `reqwest`。LLM 仅在显式配置 `KIME_LLM_ENDPOINT`
//! 时可选，缺省走规则建议，CI 完全离线。
//!
//! 设计目标：读取 TSV 语料、模拟按键、采集命中率/候选位置/翻页/回删/延迟/上下文命中，
//! 输出 rules-only 或 LLM 可消费的 JSON/Markdown 报告，并提供离线测试。
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
/// 2. 为每条语料构造临时 `kime-core` 引擎并 [`runner::Runner::run`] 逐键回放；
/// 3. 通过 [`metrics::SentenceMetric`] 汇总每句与全局指标；
/// 4. 返回 [`ProbeResult`]，可交给 [`report::ProbeReport`] 渲染 JSON/Markdown。
///
/// # Errors
/// 语料读取失败、`Dict::open` 失败或按键回放期望不满足时返回 `anyhow::Error`。
pub fn run(
    corpus_path: &std::path::Path,
    dict_path: &std::path::Path,
) -> anyhow::Result<ProbeResult> {
    let corpus = corpus::Corpus::load_tsv(corpus_path)?;
    let mut sentences = Vec::with_capacity(corpus.entries.len());
    for entry in &corpus.entries {
        let lang = runner::Lang::of_entry(entry);
        let events = key_events_for(entry, lang);
        let mut session = runner::Runner::new(dict_path, entry)?;
        session.run(&events)?;
        let metric = metrics::SentenceMetric::from_session(&entry.id, &session.trace);
        sentences.push(metric);
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

/// 把一条语料展开成按键序列：逐字符输入读音/文本，末尾空格提交。
///
/// - 中文：`pinyin` 以 `'` 分隔音节，这里丢掉分隔符只留字母（引擎按整串拼音解码）。
/// - 英文：`pinyin` 为空，退化为逐字符输入 `text` 的小写形式。
///
/// 非字母字符（标点、数字）跳过：引擎的组合缓冲只收 a-z，喂进去也只会被忽略。
fn key_events_for(entry: &corpus::CorpusEntry, lang: runner::Lang) -> Vec<runner::KeyEvent> {
    let source = match lang {
        runner::Lang::Zh if !entry.pinyin.is_empty() => entry.pinyin.as_str(),
        _ => entry.text.as_str(),
    };
    let mut events: Vec<runner::KeyEvent> = source
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| runner::KeyEvent::Char {
            ch: c.to_ascii_lowercase(),
        })
        .collect();
    events.push(runner::KeyEvent::Space);
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
