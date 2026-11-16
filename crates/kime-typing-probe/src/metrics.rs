//! 指标采集：target_hit_rate、top1_hit_rate、first_screen_hit_rate、latency p50/p99、
//! page_turns、backspace_count、reselect_count、per-lang/per-sentence 明细。
//!
//! 所有计算纯离线，基于 runner 采样的 SessionTrace，不依赖网络或用户真实词库。

use crate::runner::{Lang, SessionTrace};
use serde::{Deserialize, Serialize};

/// 逐句明细指标。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SentenceMetric {
    pub sentence_id: String,
    pub lang: Lang,
    pub text: String,
    pub pinyin: String,
    /// 是否上屏等于目标文本
    pub target_hit: bool,
    /// 是否存在某键使首候选精确等于目标文本
    pub top1_hit: bool,
    /// 目标文本是否在首屏窗内出现过
    pub first_screen_hit: bool,
    /// 按键总数
    pub keys: usize,
    /// 该句延迟统计（毫秒）
    pub latency_ms: LatencyStats,
    /// 翻页次数
    pub page_turns: usize,
    /// 退格次数
    pub backspaces: usize,
    /// 重选次数
    pub reselections: usize,
    /// 候选采样（首屏/首键等精简快照，避免报告过大）
    pub candidates_sample: Vec<String>,
}

/// 延迟分位统计。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LatencyStats {
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p99_ms: f64,
}

/// 全局汇总指标。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Summary {
    pub target_hit_rate: f64,
    pub top1_hit_rate: f64,
    pub first_screen_hit_rate: f64,
    pub latency_ms: LatencyStats,
    pub page_turns: usize,
    pub backspace_count: usize,
    pub reselect_count: usize,
    /// 综合得分 0-100（加权：命中率 60%、延迟 20%、操作成本 20%）
    pub overall_score: f64,
}

/// 按语言分桶的指标副本。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PerLang {
    pub zh: Summary,
    pub en: Summary,
}

impl SentenceMetric {
    /// 从 `SessionTrace` 构造逐句明细。
    pub fn from_session(entry_id: &str, trace: &SessionTrace) -> Self {
        let samples = &trace.samples;
        let keys = samples.len();

        // target_hit：上屏拼接 == 目标文本
        let target_hit = trace.committed == trace.text;

        // top1_hit：某键的首候选精确等于目标文本。
        // 曾用「首候选是目标前缀」判定，输入 "w" 首候选「我」就算命中「我们」，
        // 命中率虚高且与 target_hit（上屏拼接 == 目标）口径不一致。
        let top1_hit = samples
            .iter()
            .any(|s| s.candidates.first().is_some_and(|c| c == &trace.text));

        // first_screen_hit：目标文本在首屏窗内出现过
        let first_screen_hit = samples.iter().any(|s| {
            let (page_idx, page_size) = s.page;
            let start = page_idx * page_size;
            let end = start + page_size;
            s.candidates
                .get(start..end)
                .map_or(false, |window| window.iter().any(|c| c == &trace.text))
        });

        // 退格次数：KeyEvent::Backspace 的样本数
        let backspaces = samples
            .iter()
            .filter(|s| matches!(s.event, crate::runner::KeyEvent::Backspace))
            .count();

        // 候选样本：取首键首屏前 5 个候选（精简报告体积）
        let candidates_sample = samples
            .first()
            .map(|s| s.candidates.iter().take(5).cloned().collect::<Vec<_>>())
            .unwrap_or_default();

        let latencies: Vec<f64> = samples.iter().map(|s| s.latency_ms).collect();
        let latency_ms = latency_stats(&latencies);

        Self {
            sentence_id: entry_id.to_string(),
            lang: trace.lang,
            text: trace.text.clone(),
            pinyin: trace.pinyin.clone(),
            target_hit,
            top1_hit,
            first_screen_hit,
            keys,
            latency_ms,
            page_turns: trace.page_turns,
            backspaces,
            reselections: trace.reselections,
            candidates_sample,
        }
    }
}

/// 计算延迟分位：mean / p50 / p99。
fn latency_stats(latencies: &[f64]) -> LatencyStats {
    if latencies.is_empty() {
        return LatencyStats {
            mean_ms: 0.0,
            p50_ms: 0.0,
            p99_ms: 0.0,
        };
    }
    let mean = latencies.iter().sum::<f64>() / latencies.len() as f64;
    let mut sorted = latencies.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p50 = percentile(&sorted, 0.50);
    let p99 = percentile(&sorted, 0.99);
    LatencyStats {
        mean_ms: mean,
        p50_ms: p50,
        p99_ms: p99,
    }
}

/// 有序切片上的分位数（线性插值）。
fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = q * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let f = rank - lo as f64;
    sorted[lo] * (1.0 - f) + sorted[hi] * f
}

impl Summary {
    /// 从逐句明细聚合全局汇总。
    pub fn from_sentences(sentences: &[SentenceMetric]) -> Self {
        let n = sentences.len().max(1);
        let target_hit = sentences.iter().filter(|s| s.target_hit).count() as f64 / n as f64;
        let top1_hit = sentences.iter().filter(|s| s.top1_hit).count() as f64 / n as f64;
        let first_screen =
            sentences.iter().filter(|s| s.first_screen_hit).count() as f64 / n as f64;

        let all_latencies: Vec<f64> = sentences
            .iter()
            .flat_map(|s| std::iter::repeat(s.latency_ms.mean_ms).take(s.keys.max(1)))
            .collect();
        let latency_ms = latency_stats(&all_latencies);

        let page_turns = sentences.iter().map(|s| s.page_turns).sum();
        let backspace_count = sentences.iter().map(|s| s.backspaces).sum();
        let reselect_count = sentences.iter().map(|s| s.reselections).sum();

        // 综合得分 0-100：命中率 60%、延迟 20%、操作成本 20%
        let latency_penalty = (latency_ms.p99_ms / 100.0).clamp(0.0, 1.0) * 20.0;
        let op_penalty =
            ((backspace_count + reselect_count) as f64 / n as f64).clamp(0.0, 1.0) * 20.0;
        let hit_score = (target_hit + top1_hit + first_screen) / 3.0 * 60.0;
        let overall_score = (hit_score - latency_penalty - op_penalty).clamp(0.0, 100.0);

        Self {
            target_hit_rate: target_hit,
            top1_hit_rate: top1_hit,
            first_screen_hit_rate: first_screen,
            latency_ms,
            page_turns,
            backspace_count,
            reselect_count,
            overall_score,
        }
    }
}

impl PerLang {
    /// 按 lang 分桶聚合（zh / en）。
    pub fn from_sentences(sentences: &[SentenceMetric]) -> Self {
        // 保持 owned 切片：`Summary::from_sentences` 收 `&[SentenceMetric]`，
        // 传 `Vec<&SentenceMetric>` 会类型不匹配（CI 实测）。
        let zh: Vec<SentenceMetric> = sentences
            .iter()
            .filter(|s| s.lang == Lang::Zh)
            .cloned()
            .collect();
        let en: Vec<SentenceMetric> = sentences
            .iter()
            .filter(|s| s.lang == Lang::En)
            .cloned()
            .collect();
        Self {
            zh: Summary::from_sentences(&zh),
            en: Summary::from_sentences(&en),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_stats_empty_is_zero() {
        let s = latency_stats(&[]);
        assert_eq!(s.mean_ms, 0.0);
        assert_eq!(s.p50_ms, 0.0);
        assert_eq!(s.p99_ms, 0.0);
    }

    #[test]
    fn percentile_interpolates() {
        let v = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&v, 0.5), 3.0);
        assert_eq!(percentile(&v, 0.0), 1.0);
        assert_eq!(percentile(&v, 1.0), 5.0);
    }

    #[test]
    fn summary_from_empty_sentences() {
        let s = Summary::from_sentences(&[]);
        assert_eq!(s.target_hit_rate, 0.0);
        assert_eq!(s.overall_score, 0.0);
    }
}
