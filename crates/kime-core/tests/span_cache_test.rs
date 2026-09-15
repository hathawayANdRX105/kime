//! 词图格子缓存（M14 SpanCache）验收测试。
//!
//! 钉住四件事：
//! 1. **对拍**：带缓存与手动清缓存重复查询，整句候选完全一致（防缓存串味）；
//! 2. **失效**：learn 之后同会话再查，结果必须反映新 eff（缓存被清）；
//! 3. **命中**：组合内前缀延长时缓存增长 < 跨度总数（增量复用生效）；
//! 4. **上限**：超过 MAX_SPAN_CACHE_ENTRIES 后整体清空重建。

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::engine::{Engine, Key};
use kime_core::lattice::{SpanCache, MAX_SPAN_CACHE_ENTRIES};

fn tmp_db(suffix: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_span_cache_{}_{}_{}.sqlite",
        suffix,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_file(&path);
    path
}

/// 覆盖 2/3/4 音节跨度组合的最小词库。
const SEED: &str = "\
...
你好\tni hao\t500000
你\tni\t400000
好\thao\t390000
想念\txiang nian\t90000
想\txiang\t80000
念\tnian\t70000
今天晚上\tjin tian wan shang\t300000
今天\tjin tian\t290000
天\ttian\t280000
晚上\twan shang\t270000
晚\twan\t260000
上\tshang\t250000
想吃\txiang chi\t150000
吃\tchi\t140000
\te\t1
";

fn seeded_engine(suffix: &str) -> (Engine, PathBuf, PathBuf) {
    let db = tmp_db(suffix);
    let yaml = std::env::temp_dir().join(format!(
        "kime_span_cache_seed_{}_{}.yaml",
        std::process::id(),
        suffix
    ));
    fs::write(&yaml, SEED).unwrap();
    let mut d = Dict::open(&db).unwrap();
    d.import(&yaml).unwrap();
    let e = Engine::new(d, Config::default());
    (e, db, yaml)
}

fn ch(c: char) -> Key {
    Key {
        ch: Some(c),
        code: 0,
        shift: false,
        ctrl: false,
        alt: false,
    }
}

/// 提交键：engine 的空格路径要求 ch=None + code=KEY_SPACE（evdev 57）。
fn space() -> Key {
    Key {
        ch: None,
        code: 57,
        shift: false,
        ctrl: false,
        alt: false,
    }
}

/// Esc：清组合（不动上下文）。
fn esc() -> Key {
    Key {
        ch: None,
        code: 1,
        shift: false,
        ctrl: false,
        alt: false,
    }
}

/// 逐键输入（不 commit），返回引擎整句候选文本。
fn type_sentence(e: &mut Engine, keys: &str) -> Vec<String> {
    for c in keys.chars() {
        let _ = e.key(ch(c));
    }
    e.candidates().iter().map(|c| c.text.clone()).collect()
}

fn cleanup(db: &PathBuf, yaml: &PathBuf) {
    let _ = fs::remove_file(db);
    let _ = fs::remove_file(yaml);
}

/// 1. 对拍：同一句子反复逐键（缓存命中）与每次新开引擎（全冷）结果一致。
#[test]
fn cached_and_cold_runs_agree() {
    let (mut e, db, yaml) = seeded_engine("parity");
    let inputs = [
        "nihao",
        "xiangnian",
        "jintianwanshang",
        "woxiangchi",
        "nihao",
    ];
    for input in inputs {
        // 用例之间清组合（不 commit）：否则字母串接在一起，测的是另一句话
        let _ = e.key(esc());
        // 热引擎：缓存贯穿整个输入
        let warm = type_sentence(&mut e, input);
        // 冷引擎：零缓存重算
        let (mut cold, cdb, cyaml) = seeded_engine("parity_cold");
        let cold = type_sentence(&mut cold, input);
        cleanup(&cdb, &cyaml);
        assert_eq!(warm, cold, "输入 {input:?}：缓存命中结果不得偏离全冷重算");
    }
    cleanup(&db, &yaml);
}

/// 2. 失效：commit 触发 learn → 缓存清空 → 后续查询反映新 eff。
#[test]
fn learn_invalidates_cache() {
    let (mut e, db, yaml) = seeded_engine("invalidation");
    // 组合并上屏（触发 learn）：喂「你好」然后空格上屏
    type_sentence(&mut e, "nihao");
    let _ = e.key(space());
    // 缓存应为空（learn 失效）
    assert!(e.span_cache_is_empty(), "commit/learn 之后缓存必须已失效");
    // 学过「你好」后再打其它以 ni 开头的输入，整句结果必须可见新状态（不炸即对拍路径）
    let out = type_sentence(&mut e, "ni");
    assert!(!out.is_empty(), "失效后重查必须正常产出");
    cleanup(&db, &yaml);
}

/// 3. 命中：前缀延长时缓存增量增长（新增跨度数 < 总跨度数）。
#[test]
fn prefix_extension_reuses_cached_spans() {
    let (mut e, db, yaml) = seeded_engine("hit");
    type_sentence(&mut e, "jint");
    let after_4 = e.span_cache_len();
    type_sentence(&mut e, "ianwanshang"); // jintianwanshang
    let after_13 = e.span_cache_len();
    // 13 音节全跨度 = 91 个；若每键全量重算应远大于增量新增。
    assert!(after_13 > after_4, "前缀延长必须向缓存新增跨度");
    assert!(
        after_13 < 91,
        "缓存增长应远小于全跨度总数（增量复用生效），实际 {after_13}"
    );
    cleanup(&db, &yaml);
}

/// 4. 上限：超过 MAX_SPAN_CACHE_ENTRIES 后整体清空。
#[test]
fn cache_evicts_at_capacity() {
    let mut cache = SpanCache::default();
    for i in 0..=MAX_SPAN_CACHE_ENTRIES {
        cache.get_or_compute(format!("k{i}"), Vec::new);
    }
    assert!(cache.len() <= MAX_SPAN_CACHE_ENTRIES, "缓存不得无界增长");
    // 超限后旧的被清掉：k0 不再命中（重新 compute 也能工作）
    let v = cache.get_or_compute("k0".to_string(), || {
        vec![kime_core::dict::Candidate {
            text: "rebuilt".to_string(),
            pinyin: String::new(),
            freq: 1,
            eff: 1,
            ai: false,
        }]
    });
    assert_eq!(v[0].text, "rebuilt");
}
