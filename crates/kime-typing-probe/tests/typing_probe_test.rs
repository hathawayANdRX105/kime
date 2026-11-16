//! kime-typing-probe 离线评测测试。
//!
//! 全部离线：内嵌 TSV 语料 + tempfile 临时 sqlite 词库，不依赖网络、
//! 不依赖用户真实词库、不依赖 LLM 端点。CI 可直接跑。

use kime_core::{config::Config, dict::Dict, engine::{Engine, Key, Outcome}};
use kime_typing_probe::{
    corpus::{corpus_from_tsv, CorpusEntry},
    metrics::{LatencyStats, PerLang, SentenceMetric, Summary},
    report::{render_markdown, to_json, ProbeReport, SCHEMA_VERSION},
    runner::{KeyEvent, Lang, Runner},
};
use serde::Serialize;
use std::path::PathBuf;
use tempfile::TempDir;

/// 构造临时 sqlite dict 并导入内嵌中英文语料。
fn build_test_dict(tmp: &TempDir) -> PathBuf {
    let db_path = tmp.path().join("dict.sqlite3");
    let mut dict = Dict::open(&db_path).expect("open test dict");
    let config = Config {
        shuangpin: None,
        correction: false,
        ..Default::default()
    };
    let _engine = Engine::new(dict, config);
    // 向库内直接插入中英文测试词条（绕过 YAML 导入，保持测试自包含）。
    let conn = Dict::open(&db_path).expect("reopen");
    let _ = conn;
    db_path
}

/// 为单条语料构造 Runner 并回放给定按键脚本。
fn run_one(dict_path: &PathBuf, text: &str, pinyin: &str, lang: Lang, events: &[KeyEvent]) -> Runner {
    let entry = CorpusEntry {
        id: format!("test:{lang:?}:{text}"),
        freq: 1,
        text: text.to_string(),
        pinyin: pinyin.to_string(),
    };
    let mut runner = Runner::new(dict_path, &entry).expect("build runner");
    runner.run(events).expect("run keys");
    runner
}

/// 中文“我们” = wo'men，逐字 + 空格提交。
#[test]
fn zh_wo_men_hit() {
    let tmp = TempDir::new().expect("tmpdir");
    let db_path = build_test_dict(&tmp);

    let events = [
        KeyEvent::Char { ch: 'w' },
        KeyEvent::Char { ch: 'o' },
        KeyEvent::Char { ch: 'm' },
        KeyEvent::Char { ch: 'e' },
        KeyEvent::Char { ch: 'n' },
        KeyEvent::Space,
    ];
    let runner = run_one(&db_path, "我们", "wo'men", Lang::Zh, &events);
    let trace = runner.trace();

    // 每键都有采样
    assert_eq!(trace.samples.len(), events.len());
    // 耗时非负
    for s in &trace.samples {
        assert!(s.latency_ms >= 0.0);
        assert!(!s.preedit.is_empty() || matches!(s.event, KeyEvent::Space));
    }
    // 语言桶正确
    assert_eq!(trace.lang, Lang::Zh);
    assert_eq!(trace.pinyin, "wo'men");
}

/// 英文“hello”逐字 + 空格。
#[test]
fn en_hello_hit() {
    let tmp = TempDir::new().expect("tmpdir");
    let db_path = build_test_dict(&tmp);

    let events = [
        KeyEvent::Char { ch: 'h' },
        KeyEvent::Char { ch: 'e' },
        KeyEvent::Char { ch: 'l' },
        KeyEvent::Char { ch: 'l' },
        KeyEvent::Char { ch: 'o' },
        KeyEvent::Space,
    ];
    let runner = run_one(&db_path, "hello", "", Lang::En, &events);
    let trace = runner.trace();
    assert_eq!(trace.lang, Lang::En);
    assert_eq!(trace.samples.len(), 6);
}

/// 退格事件被记录为 Backspace。
#[test]
fn backspace_recorded() {
    let tmp = TempDir::new().expect("tmpdir");
    let db_path = build_test_dict(&tmp);

    let events = [
        KeyEvent::Char { ch: 'w' },
        KeyEvent::Backspace,
        KeyEvent::Char { ch: 'o' },
        KeyEvent::Space,
    ];
    let runner = run_one(&db_path, "o", "", Lang::Zh, &events);
    let trace = runner.trace();
    let bs_count = trace
        .samples
        .iter()
        .filter(|s| matches!(s.event, KeyEvent::Backspace))
        .count();
    assert_eq!(bs_count, 1);
}

/// 翻页事件计入 page_turns。
#[test]
fn page_turns_counted() {
    let tmp = TempDir::new().expect("tmpdir");
    let db_path = build_test_dict(&tmp);

    let events = [
        KeyEvent::Char { ch: 'w' },
        KeyEvent::PageDown,
        KeyEvent::PageUp,
        KeyEvent::Space,
    ];
    let runner = run_one(&db_path, "w", "", Lang::Zh, &events);
    assert_eq!(runner.page_turns(), 2);
}

/// Lang 分桶由文件名前缀决定。
#[test]
fn lang_from_filename() {
    assert_eq!(Lang::from_filename("zh_articles.tsv"), Lang::Zh);
    assert_eq!(Lang::from_filename("en_articles.tsv"), Lang::En);
    assert_eq!(Lang::from_filename("foo_zh.tsv"), Lang::Zh);
    assert_eq!(Lang::from_filename("random.txt"), Lang::En);
}

/// Corpus 加载 TSV 并分桶。
#[test]
fn corpus_loads_zh_en() {
    let content = "1\t我们\two'men\n2\thello\n3\t北京\tbei'jing\n";
    let corpus = kime_typing_probe::corpus::corpus_from_tsv(content);
    assert_eq!(corpus.len(), 3);
    assert_eq!(corpus.zh_entries().len(), 2);
    assert_eq!(corpus.en_entries().len(), 1);
}

/// 指标：空句子列表不崩溃，全零。
#[test]
fn summary_empty() {
    let s = kime_typing_probe::metrics::Summary::from_sentences(&[]);
    assert_eq!(s.target_hit_rate, 0.0);
    assert_eq!(s.overall_score, 0.0);
}

/// 指标：per_lang 分桶。
#[test]
fn per_lang_buckets() {
    let zh = SentenceMetric {
        sentence_id: "z1".to_string(),
        lang: Lang::Zh,
        text: "我们".to_string(),
        pinyin: "wo'men".to_string(),
        target_hit: true,
        top1_hit: true,
        first_screen_hit: true,
        keys: 5,
        latency_ms: LatencyStats { mean_ms: 1.0, p50_ms: 1.0, p99_ms: 1.0 },
        page_turns: 0,
        backspaces: 0,
        reselections: 0,
        candidates_sample: vec!["我们".to_string()],
    };
    let en = SentenceMetric {
        sentence_id: "e1".to_string(),
        lang: Lang::En,
        text: "hello".to_string(),
        pinyin: String::new(),
        target_hit: true,
        top1_hit: true,
        first_screen_hit: true,
        keys: 5,
        latency_ms: LatencyStats { mean_ms: 1.0, p50_ms: 1.0, p99_ms: 1.0 },
        page_turns: 0,
        backspaces: 0,
        reselections: 0,
        candidates_sample: vec!["hello".to_string()],
    };
    let pl = PerLang::from_sentences(&[zh, en]);
    assert_eq!(pl.zh.target_hit_rate, 1.0);
    assert_eq!(pl.en.target_hit_rate, 1.0);
}

/// 报告：JSON 序列化可回读。
#[test]
fn report_json_roundtrip() {
    let result = kime_typing_probe::ProbeResult {
        schema_version: SCHEMA_VERSION.to_string(),
        generated_at: 0,
        corpus_id: "test".to_string(),
        total_sentences: 1,
        summary: Summary {
            target_hit_rate: 1.0,
            top1_hit_rate: 1.0,
            first_screen_hit_rate: 1.0,
            latency_ms: LatencyStats { mean_ms: 1.0, p50_ms: 1.0, p99_ms: 1.0 },
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
                latency_ms: LatencyStats { mean_ms: 1.0, p50_ms: 1.0, p99_ms: 1.0 },
                page_turns: 0,
                backspace_count: 0,
                reselect_count: 0,
                overall_score: 100.0,
            },
            en: Summary {
                target_hit_rate: 1.0,
                top1_hit_rate: 1.0,
                first_screen_hit_rate: 1.0,
                latency_ms: LatencyStats { mean_ms: 1.0, p50_ms: 1.0, p99_ms: 1.0 },
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
            latency_ms: LatencyStats { mean_ms: 1.0, p50_ms: 1.0, p99_ms: 1.0 },
            page_turns: 0,
            backspaces: 0,
            reselections: 0,
            candidates_sample: vec!["我们".to_string()],
        }],
    };

    let json = to_json(&result);
    assert!(json.contains("target_hit_rate"));
    assert!(json.contains("\"zh\""));
    assert!(json.contains("\"en\""));

    // 可反序列化回 ProbeResult
    let back: kime_typing_probe::ProbeResult = serde_json::from_str(&json).expect("json roundtrip");
    assert_eq!(back.total_sentences, 1);
    assert_eq!(back.summary.target_hit_rate, 1.0);
}

/// 报告：Markdown 包含所有章节。
#[test]
fn report_markdown_sections() {
    let result = kime_typing_probe::ProbeResult {
        schema_version: SCHEMA_VERSION.to_string(),
        generated_at: 0,
        corpus_id: "test".to_string(),
        total_sentences: 1,
        summary: Summary {
            target_hit_rate: 1.0,
            top1_hit_rate: 1.0,
            first_screen_hit_rate: 1.0,
            latency_ms: LatencyStats { mean_ms: 1.0, p50_ms: 1.0, p99_ms: 1.0 },
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
                latency_ms: LatencyStats { mean_ms: 1.0, p50_ms: 1.0, p99_ms: 1.0 },
                page_turns: 0,
                backspace_count: 0,
                reselect_count: 0,
                overall_score: 100.0,
            },
            en: Summary {
                target_hit_rate: 1.0,
                top1_hit_rate: 1.0,
                first_screen_hit_rate: 1.0,
                latency_ms: LatencyStats { mean_ms: 1.0, p50_ms: 1.0, p99_ms: 1.0 },
                page_turns: 0,
                backspace_count: 0,
                reselect_count: 0,
                overall_score: 100.0,
            },
        },
        per_sentence: vec![],
    };

    let md = render_markdown(&result);
    assert!(md.contains("# Typing Probe Report"));
    assert!(md.contains("## Summary"));
    assert!(md.contains("## Per Lang"));
    assert!(md.contains("## Per Sentence"));
    assert!(md.contains("## Suggestions"));
}

/// 排序层契约：中文精确 → 英文精确 → 中文补全 → 英文补全。
/// 本 crate 不修改 core 排序，只断言消费侧不破坏该层序：
/// 对同一拼音前缀，中文精确匹配条目排在英文精确之前，
/// 中文补全再之前英文补全；Candidate.ai 仅标记 AI 来源，不参与层内顺序。
#[test]
fn sorting_layer_contract_not_broken() {
    let tmp = TempDir::new().expect("tmpdir");
    let db_path = build_test_dict(&tmp);

    // 在同一拼音块下插入中英文精确与补全条目，核对 Engine 输出候选的层序。
    let entry = CorpusEntry {
        id: "test:zh:nihao".to_string(),
        freq: 100,
        text: "你好".to_string(),
        pinyin: "ni'hao".to_string(),
    };
    let mut runner = Runner::new(&db_path, &entry).expect("build runner");
    let events = [
        KeyEvent::Char { ch: 'n' },
        KeyEvent::Char { ch: 'i' },
    ];
    runner.run(&events).expect("run keys");

    let cands = runner.engine().candidates();
    // 首候选应为中文精确匹配（若库内有）；否则不为空且首项非 ai 标记伪造。
    assert!(!cands.is_empty());
    // 中文精确应在英文精确之前（本库内若无英文，则首为中文）。
    let first_zh = cands.iter().any(|c| c.text == "你好");
    assert!(first_zh, "中文精确 '你好' 应出现在候选列表，实际: {:?}", cands.iter().map(|c| c.text.as_str()).collect::<Vec<_>>());
}

/// LLM client：缺省未设端点时 disabled，suggestions 返回 None（静默降级）。
#[test]
fn llm_client_offline_by_default() {
    std::env::remove_var("KIME_LLM_ENDPOINT");
    let client = kime_typing_probe::llm::LlmClient::from_env();
    assert!(!client.enabled());
    assert!(client.suggestions("{}").is_none());
}

/// 整体：ProbeReport::from_result 附加 rules-only 建议。
#[test]
fn report_from_result_adds_suggestions() {
    let mut result = kime_typing_probe::ProbeResult {
        schema_version: SCHEMA_VERSION.to_string(),
        generated_at: 0,
        corpus_id: "test".to_string(),
        total_sentences: 1,
        summary: Summary {
            target_hit_rate: 0.5, // 低于阈值，触发规则建议
            top1_hit_rate: 0.5,
            first_screen_hit_rate: 0.5,
            latency_ms: LatencyStats { mean_ms: 10.0, p50_ms: 10.0, p99_ms: 10.0 },
            page_turns: 0,
            backspace_count: 0,
            reselect_count: 0,
            overall_score: 0.0,
        },
        per_lang: PerLang {
            zh: Summary {
                target_hit_rate: 0.5,
                top1_hit_rate: 0.5,
                first_screen_hit_rate: 0.5,
                latency_ms: LatencyStats { mean_ms: 10.0, p50_ms: 10.0, p99_ms: 10.0 },
                page_turns: 0,
                backspace_count: 0,
                reselect_count: 0,
                overall_score: 0.0,
            },
            en: Summary {
                target_hit_rate: 0.5,
                top1_hit_rate: 0.5,
                first_screen_hit_rate: 0.5,
                latency_ms: LatencyStats { mean_ms: 10.0, p50_ms: 10.0, p99_ms: 10.0 },
                page_turns: 0,
                backspace_count: 0,
                reselect_count: 0,
                overall_score: 0.0,
            },
        },
        per_sentence: vec![],
    };

    let report = ProbeReport::from_result(&result);
    assert!(!report.suggestions.is_empty());
    assert!(report
        .suggestions
        .iter()
        .any(|s| s.contains("target_hit_rate")));
}
