//! kime-typing-probe 离线评测测试。
//!
//! 全部离线：内嵌 TSV 语料 + tempfile 临时 sqlite 词库，不依赖网络、
//! 不依赖用户真实词库、不依赖 LLM 端点。CI 可直接跑。

use kime_core::dict::Dict;
use kime_typing_probe::{
    corpus::CorpusEntry,
    metrics::{LatencyStats, PerLang, SentenceMetric, Summary},
    report::{render_markdown, to_json, ProbeReport, SCHEMA_VERSION},
    runner::{KeyEvent, Lang, Runner},
};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const CN_DICT: &str = "\
...
你好	ni hao	9000000
你	ni	9500000
好	hao	9000000
我们	wo men	8000000
我	wo	9200000
们	men	7000000
挖	wa	5000000
哇	wa	4900000
娃	wa	4800000
瓦	wa	4700000
袜	wa	4600000
歪	wai	5100000
外	wai	5000000
弯	wan	4900000
完	wan	4800000
万	wan	4700000
王	wang	5200000
往	wang	5100000
网	wang	5000000
为	wei	5300000
位	wei	5200000
文	wen	5400000
问	wen	5300000
卧	wo	4000000
屋	wu	5500000
无	wu	5400000
五	wu	5300000
物	wu	5200000
";

const EN_DICT: &str = "\
...
hello	hello	900000
world	world	800000
good	good	700000
";

/// 构造临时 sqlite dict 并导入中英文测试词条。
///
/// 空词库会让翻页/候选类断言全部落空（引擎无候选时按键返回 Ignored），
/// 所以这里必须真的写入词条——走 `Dict::import` / `Dict::import_english`
/// 的正规导入路径，与 `kime-core` 其它集成测试同款。
fn build_test_dict(tmp: &TempDir) -> PathBuf {
    let db_path = tmp.path().join("dict.sqlite3");
    let cn_yaml = tmp.path().join("cn.yaml");
    let en_yaml = tmp.path().join("en.yaml");
    std::fs::write(&cn_yaml, CN_DICT).expect("write cn yaml");
    std::fs::write(&en_yaml, EN_DICT).expect("write en yaml");
    let mut dict = Dict::open(&db_path).expect("open test dict");
    dict.import(&cn_yaml).expect("import cn dict");
    dict.import_english(&en_yaml).expect("import en dict");
    db_path
}

/// 用调用方给的 YAML 建库（自造最小词库，避免依赖共享 `CN_DICT` 里恰好有哪些词）。
fn build_dict(tmp: &TempDir, tag: &str, cn: &str, en: &str) -> PathBuf {
    let db_path = tmp.path().join(format!("{tag}.sqlite3"));
    let cn_yaml = tmp.path().join(format!("{tag}-cn.yaml"));
    let en_yaml = tmp.path().join(format!("{tag}-en.yaml"));
    std::fs::write(&cn_yaml, cn).expect("write cn yaml");
    std::fs::write(&en_yaml, en).expect("write en yaml");
    let mut dict = Dict::open(&db_path).expect("open dict");
    dict.import(&cn_yaml).expect("import cn dict");
    dict.import_english(&en_yaml).expect("import en dict");
    db_path
}

/// 为单条语料构造 Runner 并回放给定按键脚本。
fn run_one(dict_path: &Path, text: &str, pinyin: &str, lang: Lang, events: &[KeyEvent]) -> Runner {
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

    // 上屏文本必须等于目标（真正证明「打出来了」，而非只证明按键被消费）
    assert_eq!(trace.committed, "我们", "逐字 wo'men + 空格应上屏「我们」");
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

/// Lang 分桶由条目读音决定，不依赖文件名。
#[test]
fn lang_follows_entry_reading() {
    let zh = CorpusEntry {
        id: "path/zh_articles.tsv:1".to_string(),
        freq: 1,
        text: "我们".to_string(),
        pinyin: "wo'men".to_string(),
    };
    let en = CorpusEntry {
        id: "path/en_articles.tsv:1".to_string(),
        freq: 1,
        text: "hello".to_string(),
        pinyin: String::new(),
    };
    assert_eq!(Lang::of_entry(&zh), Lang::Zh);
    assert_eq!(Lang::of_entry(&en), Lang::En);
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
        latency_ms: LatencyStats {
            mean_ms: 1.0,
            p50_ms: 1.0,
            p99_ms: 1.0,
        },
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
        latency_ms: LatencyStats {
            mean_ms: 1.0,
            p50_ms: 1.0,
            p99_ms: 1.0,
        },
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
            latency_ms: LatencyStats {
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
                latency_ms: LatencyStats {
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
                latency_ms: LatencyStats {
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
            latency_ms: LatencyStats {
                mean_ms: 1.0,
                p50_ms: 1.0,
                p99_ms: 1.0,
            },
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
            latency_ms: LatencyStats {
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
                latency_ms: LatencyStats {
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
                latency_ms: LatencyStats {
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
        per_sentence: vec![],
    };

    let md = render_markdown(&result);
    assert!(md.contains("# Typing Probe Report"));
    assert!(md.contains("## Summary"));
    assert!(md.contains("## Per Lang"));
    assert!(md.contains("## Per Sentence"));
    assert!(md.contains("## Suggestions"));
}

/// 排序层契约：中文精确 → 中文补全；且候选层序单调不降。
///
/// 词库里有 `ni`（你/尼，精确）与 `ni hao`（你好，补全），断言：
/// 1. 候选非空；
/// 2. 层序单调不降（`Layer` 序：Exact &lt; Completion）——跨层回归直接被捕获；
/// 3. 中文精确「你」在中文补全「你好」之前。
///
/// 只断言「某候选出现过」捕捉不到跨层回归（词库缺条目时永远成立）。
#[test]
fn sorting_layer_contract_not_broken() {
    let tmp = TempDir::new().expect("tmpdir");
    let db_path = build_test_dict(&tmp);

    let entry = CorpusEntry {
        id: "test:zh:ni".to_string(),
        freq: 100,
        text: "你".to_string(),
        pinyin: "ni".to_string(),
    };
    let mut runner = Runner::new(&db_path, &entry).expect("build runner");
    let events = [KeyEvent::Char { ch: 'n' }, KeyEvent::Char { ch: 'i' }];
    runner.run(&events).expect("run keys");

    let cands = runner.engine().candidates();
    assert!(!cands.is_empty(), "候选不应为空");

    // 层序单调不降：RankKey 的 tier 在分数之前比较。
    for w in cands.windows(2) {
        assert!(
            w[0].layer <= w[1].layer,
            "层序必须单调不降：{:?} 在 {:?} 之前",
            (w[0].text.as_str(), w[0].layer),
            (w[1].text.as_str(), w[1].layer)
        );
    }

    let pos = |t: &str| cands.iter().position(|c| c.text == t);
    if let (Some(zh_exact), Some(zh_comp)) = (pos("你"), pos("你好")) {
        assert!(
            zh_exact < zh_comp,
            "中文精确「你」应在中文补全「你好」之前：{:?}",
            cands.iter().map(|c| c.text.as_str()).collect::<Vec<_>>()
        );
    }
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
    let result = kime_typing_probe::ProbeResult {
        schema_version: SCHEMA_VERSION.to_string(),
        generated_at: 0,
        corpus_id: "test".to_string(),
        total_sentences: 1,
        summary: Summary {
            target_hit_rate: 0.5, // 低于阈值，触发规则建议
            top1_hit_rate: 0.5,
            first_screen_hit_rate: 0.5,
            latency_ms: LatencyStats {
                mean_ms: 10.0,
                p50_ms: 10.0,
                p99_ms: 10.0,
            },
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
                latency_ms: LatencyStats {
                    mean_ms: 10.0,
                    p50_ms: 10.0,
                    p99_ms: 10.0,
                },
                page_turns: 0,
                backspace_count: 0,
                reselect_count: 0,
                overall_score: 0.0,
            },
            en: Summary {
                target_hit_rate: 0.5,
                top1_hit_rate: 0.5,
                first_screen_hit_rate: 0.5,
                latency_ms: LatencyStats {
                    mean_ms: 10.0,
                    p50_ms: 10.0,
                    p99_ms: 10.0,
                },
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

/// 端到端：`run()` 真的回放按键、产出非零指标与可序列化报告。
///
/// 回归的 bug：`run` 曾以空事件调用 `Runner::run`，所有命中率恒为 false、
/// 计数恒为 0，头部 API 形同虚设；这条测试钉住「语料 → 按键 → 指标」链路。
#[test]
fn run_replays_corpus_end_to_end() {
    let tmp = TempDir::new().expect("tmpdir");
    let db_path = build_test_dict(&tmp);
    let corpus_path = tmp.path().join("zh_articles.tsv");
    std::fs::write(&corpus_path, "1\t我们\two'men\n").expect("write corpus");

    let result =
        kime_typing_probe::run(&corpus_path, &db_path, Default::default()).expect("run probe");

    assert_eq!(result.total_sentences, 1);
    let sentence = &result.per_sentence[0];
    // 按键序列来自读音（w o m e n + 空格），不是空
    assert_eq!(sentence.keys, 6);
    // 目标命中：上屏文本等于语料文本
    assert!(
        sentence.target_hit,
        "committed={:?}",
        sentence.candidates_sample
    );
    // 语言桶：读音非空 → zh
    assert_eq!(sentence.lang, Lang::Zh);
    assert_eq!(result.per_lang.zh.target_hit_rate, 1.0);
    // 报告可序列化
    let md = result.to_markdown();
    assert!(md.contains("## Summary"));
    let json = result.to_json();
    assert!(json.contains("target_hit_rate"));
}

/// 语料里的 `# article: <标题>` 把条目 id 变成 `<标题>#<行号>`，报告里能区分文章。
#[test]
fn article_directive_groups_entries() {
    let content = "# article: 日常\n1\t我们\two'men\n# article: 校园\n1\t学校\txue'xiao\n";
    let corpus = kime_typing_probe::corpus::corpus_from_tsv(content);
    assert_eq!(corpus.len(), 2);
    assert_eq!(corpus.entries[0].id, "日常#2");
    assert_eq!(corpus.entries[1].id, "校园#4");
}

/// 仓库自带的 fixture 必须能被同一份解析器读出来（防止手改 fixture 时改坏格式）。
#[test]
fn shipped_fixtures_parse() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let zh = kime_typing_probe::corpus::Corpus::load_tsv(&dir.join("zh_articles.tsv"))
        .expect("zh fixture must parse");
    let en = kime_typing_probe::corpus::Corpus::load_tsv(&dir.join("en_articles.tsv"))
        .expect("en fixture must parse");
    assert!(!zh.is_empty(), "中文 fixture 不应为空");
    assert!(!en.is_empty(), "英文 fixture 不应为空");
    // lang 判据是「读音是否为空」，与文件名无关。
    assert_eq!(zh.zh_entries().len(), zh.len());
    assert_eq!(en.en_entries().len(), en.len());
}

/// 配置透传：语料用**双拼键串**（自然码 `ren` = `rf`）时，`shuangpin: Some(Ziranma)`
/// 必须解出「人」。
///
/// 语料里的键是「按哪种方案敲出来的键」，必须和 `--mode` 一致。这条钉住 `Config`
/// 真的透传到了引擎——写死 `shuangpin: None` 时 `rf` 不是任何全拼音节，候选为空。
///
/// 选 `ren`/`rf` 而不是 `shi`/`ui`：后者在引擎里会同时触发「声母位 + 半截键」合批，
/// 候选列表里混进别的音节的词，断言就不再只表达「解码对不对」。
#[test]
fn ziranma_config_decodes_ziranma_corpus() {
    let tmp = TempDir::new().expect("tmpdir");
    // 自带最小词库：共享 CN_DICT 里没有「人」，借它会让候选恒为空、断言失去意义。
    let db_path = build_dict(&tmp, "ren", "...\n人\tren\t9000000\n", "...\n");
    let corpus_path = tmp.path().join("zh_articles.tsv");
    std::fs::write(&corpus_path, "1\t人\trf\n").expect("write corpus");

    let config = kime_core::config::Config {
        shuangpin: Some(kime_core::Scheme::Ziranma),
        ..Default::default()
    };
    let result = kime_typing_probe::run(&corpus_path, &db_path, config).expect("run probe");

    let sentence = &result.per_sentence[0];
    assert!(
        sentence.target_hit,
        "双拼音料 + 双拼配置应上屏「人」：{:?}",
        sentence.candidates_sample
    );
}

/// 全拼音料 + 默认（全拼）配置同样命中：默认路径不能被双拼那条带坏。
#[test]
fn full_config_decodes_full_pinyin_corpus() {
    let tmp = TempDir::new().expect("tmpdir");
    let db_path = build_dict(&tmp, "ren2", "...\n人\tren\t9000000\n", "...\n");
    let corpus_path = tmp.path().join("zh_articles.tsv");
    std::fs::write(&corpus_path, "1\t人\tren\n").expect("write corpus");

    let result =
        kime_typing_probe::run(&corpus_path, &db_path, Default::default()).expect("run probe");
    let sentence = &result.per_sentence[0];
    assert!(
        sentence.target_hit,
        "全拼音料 + 全拼配置应上屏「人」：{:?}",
        sentence.candidates_sample
    );
}

/// 整份语料共用一个引擎：连续回放时每条都能上屏目标，没有因为残留组合而错位。
///
/// 每条新建引擎会让上下文加成恒为零（probe 就测不出「前一个词影响下一个词」），
/// 所以这里用「两条语料连续回放」钉住引擎复用这条路径真的能跑通。
#[test]
fn engine_is_shared_across_entries() {
    let tmp = TempDir::new().expect("tmpdir");
    let db_path = build_test_dict(&tmp);
    let corpus_path = tmp.path().join("zh_articles.tsv");
    std::fs::write(&corpus_path, "1\t我们\two'men\n2\t我\two\n").expect("write corpus");

    let result =
        kime_typing_probe::run(&corpus_path, &db_path, Default::default()).expect("run probe");
    assert_eq!(result.total_sentences, 2);
    assert!(
        result.per_sentence.iter().all(|s| s.target_hit),
        "连续回放应每条都上屏目标：{:?}",
        result
            .per_sentence
            .iter()
            .map(|s| (s.text.as_str(), s.target_hit, s.keys))
            .collect::<Vec<_>>()
    );
}
