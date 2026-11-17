//! 报告渲染：输出 JSON/Markdown；包含 summary/per_lang/per_sentence/suggestions；
//! rules-only 建议缺省提供，LLM 可选通过 `KIME_LLM_ENDPOINT`，缺失/失败静默降级。

use crate::metrics::{PerLang, SentenceMetric, Summary};
use crate::ProbeResult;
use serde::Serialize;

/// 报告 JSON schema 版本。
pub const SCHEMA_VERSION: &str = "1.0";

/// 可序列化的完整报告（与 [`ProbeResult`] 同结构，供外部 LLM 消费）。
#[derive(Clone, Debug, Serialize)]
pub struct ProbeReport {
    pub schema_version: String,
    pub generated_at: u64,
    pub corpus_id: String,
    pub total_sentences: usize,
    pub summary: Summary,
    pub per_lang: PerLang,
    pub per_sentence: Vec<SentenceMetric>,
    pub suggestions: Vec<String>,
}

impl ProbeReport {
    /// 从 [`ProbeResult`] 构造报告，附加 rules-only 建议。
    pub fn from_result(result: &ProbeResult) -> Self {
        let mut suggestions =
            rules_only_suggestions(&result.summary, &result.per_lang, &result.per_sentence);
        // LLM 可选：端点已配置且编译了 `llm` feature 时追加；未配置/失败静默降级。
        let summary_json =
            serde_json::to_string(&(&result.summary, &result.per_lang, result.total_sentences))
                .unwrap_or_default();
        suggestions.extend(
            crate::llm::LlmClient::from_env()
                .suggestions(&summary_json)
                .unwrap_or_default(),
        );
        Self {
            schema_version: result.schema_version.clone(),
            generated_at: result.generated_at,
            corpus_id: result.corpus_id.clone(),
            total_sentences: result.total_sentences,
            summary: result.summary.clone(),
            per_lang: result.per_lang.clone(),
            per_sentence: result.per_sentence.clone(),
            suggestions,
        }
    }

    /// 序列化为紧凑 JSON。
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }

    /// 序列化为 Markdown 文本。
    pub fn to_markdown(&self) -> String {
        render_markdown_result(self)
    }
}

/// 将 [`ProbeResult`] 序列化为紧凑 JSON（兼容 `ProbeResult::to_json`）。
pub fn to_json(result: &ProbeResult) -> String {
    let report = ProbeReport::from_result(result);
    report.to_json()
}

/// 将 [`ProbeResult`] 渲染为 Markdown（兼容 `ProbeResult::to_markdown`）。
pub fn render_markdown(result: &ProbeResult) -> String {
    let report = ProbeReport::from_result(result);
    render_markdown_result(&report)
}

/// 规则兜底建议：基于 summary/per_lang 生成中英文建议。
/// 缺省启用，LLM 失败/未配置时仅保留此部分，不报错、不阻断。
fn rules_only_suggestions(
    summary: &Summary,
    per_lang: &PerLang,
    per_sentence: &[SentenceMetric],
) -> Vec<String> {
    let mut suggestions: Vec<String> = Vec::new();

    if summary.target_hit_rate < 0.8 {
        suggestions.push(format!(
            "target_hit_rate {:.2} < 0.80：加强该拼音块精确匹配词库覆盖，优先补全高频短语。",
            summary.target_hit_rate
        ));
    }
    if summary.top1_hit_rate < 0.7 {
        suggestions.push(format!(
            "top1_hit_rate {:.2} < 0.70：首候选排序需调整，检查精确匹配层是否在补全层之前。",
            summary.top1_hit_rate
        ));
    }
    if summary.first_screen_hit_rate < 0.9 {
        suggestions.push(format!(
            "first_screen_hit_rate {:.2} < 0.90：首屏窗命中不足，考虑扩大 page_size 或提升高频词权重。",
            summary.first_screen_hit_rate
        ));
    }
    if summary.latency_ms.p99_ms > 50.0 {
        suggestions.push(format!(
            "latency p99 {:.2}ms > 50ms：单键延迟偏高，排查 dict 查询索引与排序热路径。",
            summary.latency_ms.p99_ms
        ));
    }
    if summary.backspace_count > 0 {
        suggestions.push(format!(
            "backspace_count {}：用户回删发生，检查模糊音/纠错是否误导选错候选。",
            summary.backspace_count
        ));
    }
    if summary.reselect_count > 0 {
        suggestions.push(format!(
            "reselect_count {}：同一句内重复选词，候选排序不稳定或首屏未命中。",
            summary.reselect_count
        ));
    }

    // per-lang 兜底：**只对该语种真的有语料时**才建议。
    // 纯中文语料会让 en 桶全零，旧实现据此输出「en: target_hit_rate 0.00 低于 0.80」，
    // 把「没测过」说成「测得差」——建议必须区分「零样本」与「低命中」。
    for (lang, s) in [("zh", &per_lang.zh), ("en", &per_lang.en)] {
        let samples = per_sentence
            .iter()
            .filter(|m| m.lang.as_str() == lang)
            .count();
        if samples == 0 {
            continue;
        }
        if s.target_hit_rate < 0.8 {
            suggestions.push(format!(
                "{lang}: target_hit_rate {:.2} 低于 0.80，建议专项补强该语种语料。",
                s.target_hit_rate
            ));
        }
    }

    if suggestions.is_empty() {
        suggestions.push("各指标均在阈值内，暂无规则建议。".to_string());
    }

    suggestions
}

/// Markdown 渲染（核心逻辑，供 `ProbeReport::to_markdown` 复用）。
fn render_markdown_result(report: &ProbeReport) -> String {
    let mut md = String::new();
    md.push_str("# Typing Probe Report\n\n");
    md.push_str(&format!("- Schema version: {}\n", report.schema_version));
    md.push_str(&format!(
        "- Generated at (unix seconds): {}\n",
        report.generated_at
    ));
    md.push_str(&format!("- Corpus: {}\n", report.corpus_id));
    md.push_str(&format!(
        "- Total sentences: {}\n\n",
        report.total_sentences
    ));

    md.push_str("## Summary\n\n");
    md.push_str(&format!(
        "- Target hit rate: {:.4}\n",
        report.summary.target_hit_rate
    ));
    md.push_str(&format!(
        "- Top-1 hit rate: {:.4}\n",
        report.summary.top1_hit_rate
    ));
    md.push_str(&format!(
        "- First-screen hit rate: {:.4}\n",
        report.summary.first_screen_hit_rate
    ));
    md.push_str(&format!(
        "- Latency (ms): mean={:.2}, p50={:.2}, p99={:.2}\n",
        report.summary.latency_ms.mean_ms,
        report.summary.latency_ms.p50_ms,
        report.summary.latency_ms.p99_ms
    ));
    md.push_str(&format!("- Page turns: {}\n", report.summary.page_turns));
    md.push_str(&format!(
        "- Backspace count: {}\n",
        report.summary.backspace_count
    ));
    md.push_str(&format!(
        "- Reselect count: {}\n",
        report.summary.reselect_count
    ));
    md.push_str(&format!(
        "- Overall score: {:.2}/100\n\n",
        report.summary.overall_score
    ));

    md.push_str("## Per Lang\n\n");
    md.push_str(
        "| Lang | Target Hit | Top-1 Hit | First-Screen Hit | Latency p99 (ms) | Score |\n",
    );
    md.push_str(
        "|------|------------|-----------|------------------|------------------|-------|\n",
    );
    for (lang, s) in [("zh", &report.per_lang.zh), ("en", &report.per_lang.en)] {
        md.push_str(&format!(
            "| {lang} | {:.4} | {:.4} | {:.4} | {:.2} | {:.2} |\n",
            s.target_hit_rate,
            s.top1_hit_rate,
            s.first_screen_hit_rate,
            s.latency_ms.p99_ms,
            s.overall_score,
        ));
    }
    md.push('\n');

    md.push_str("## Per Sentence\n\n");
    md.push_str("| ID | Lang | Target | Top-1 | First-Screen | Keys | Latency mean (ms) | Page | BS | Resel |\n");
    md.push_str("|----|------|--------|-------|--------------|------|-------------------|------|----|-------|\n");
    for s in &report.per_sentence {
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {:.2} | {} | {} | {} |\n",
            s.sentence_id,
            s.lang.as_str(),
            s.target_hit,
            s.top1_hit,
            s.first_screen_hit,
            s.keys,
            s.latency_ms.mean_ms,
            s.page_turns,
            s.backspaces,
            s.reselections,
        ));
    }
    md.push('\n');

    md.push_str("## Suggestions\n\n");
    for s in &report.suggestions {
        md.push_str(&format!("- {s}\n"));
    }
    md.push('\n');

    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::Lang;

    fn sample_result() -> ProbeResult {
        ProbeResult {
            schema_version: SCHEMA_VERSION.to_string(),
            generated_at: 0,
            corpus_id: "test".to_string(),
            total_sentences: 1,
            summary: Summary {
                target_hit_rate: 1.0,
                top1_hit_rate: 1.0,
                first_screen_hit_rate: 1.0,
                latency_ms: crate::metrics::LatencyStats {
                    mean_ms: 1.0,
                    p50_ms: 1.0,
                    p99_ms: 1.0,
                },
                page_turns: 0,
                backspace_count: 0,
                reselect_count: 0,
                overall_score: 100.0,
            },
            per_lang: PerLang {
                zh: Summary {
                    target_hit_rate: 1.0,
                    top1_hit_rate: 1.0,
                    first_screen_hit_rate: 1.0,
                    latency_ms: crate::metrics::LatencyStats {
                        mean_ms: 1.0,
                        p50_ms: 1.0,
                        p99_ms: 1.0,
                    },
                    page_turns: 0,
                    backspace_count: 0,
                    reselect_count: 0,
                    overall_score: 100.0,
                },
                en: Summary {
                    target_hit_rate: 1.0,
                    top1_hit_rate: 1.0,
                    first_screen_hit_rate: 1.0,
                    latency_ms: crate::metrics::LatencyStats {
                        mean_ms: 1.0,
                        p50_ms: 1.0,
                        p99_ms: 1.0,
                    },
                    page_turns: 0,
                    backspace_count: 0,
                    reselect_count: 0,
                    overall_score: 100.0,
                },
            },
            per_sentence: vec![SentenceMetric {
                sentence_id: "s1".to_string(),
                lang: Lang::Zh,
                text: "我们".to_string(),
                pinyin: "wo'men".to_string(),
                target_hit: true,
                top1_hit: true,
                first_screen_hit: true,
                keys: 5,
                latency_ms: crate::metrics::LatencyStats {
                    mean_ms: 1.0,
                    p50_ms: 1.0,
                    p99_ms: 1.0,
                },
                page_turns: 0,
                backspaces: 0,
                reselections: 0,
                candidates_sample: vec!["我们".to_string()],
            }],
        }
    }

    #[test]
    fn to_json_roundtrip() {
        let result = sample_result();
        let json = to_json(&result);
        assert!(json.contains("target_hit_rate"));
        assert!(json.contains("\"zh\""));
    }

    #[test]
    fn markdown_contains_sections() {
        let result = sample_result();
        let md = render_markdown(&result);
        assert!(md.contains("# Typing Probe Report"));
        assert!(md.contains("## Summary"));
        assert!(md.contains("## Per Lang"));
        assert!(md.contains("## Per Sentence"));
        assert!(md.contains("## Suggestions"));
    }

    #[test]
    fn rules_suggestions_low_hit() {
        let mut result = sample_result();
        result.summary.target_hit_rate = 0.5;
        let report = ProbeReport::from_result(&result);
        assert!(report
            .suggestions
            .iter()
            .any(|s| s.contains("target_hit_rate")));
    }
}
