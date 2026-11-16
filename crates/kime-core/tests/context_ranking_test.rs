//! 实时上下文排序（跨语言统一分数重构后）：bigram + 上下文召回 + 片段挖掘。
//!
//! 验证：
//! 1. 上一词的实时 bigram 按「上一词 → 候选读音」查表，在同层内重排候选；
//! 2. 上下文不匹配时不加成（外键错配 = 恒零，这里钉死正确方向）；
//! 3. 英文提交不污染中文 bigram；
//! 4. 加成被 CONTEXT_BOOST_CAP 封顶，永不压过裸频；
//! 5. 离线片段挖掘计入 MineStats.fragments_mined。
//!
//! 不引入新依赖，沿用 main 风格（临时 sqlite + Dict::open）。

use kime_core::dict::{Dict, CONTEXT_BOOST_CAP};
use kime_core::lm;
use std::fs;

fn tmp_db(suffix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_ctx_rank_{}_{}_{}",
        suffix,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_file(&path);
    path
}

/// 构造中文候选（Exact 层）。
fn zh(text: &str, pinyin: &str) -> kime_core::dict::Candidate {
    kime_core::dict::Candidate::zh(text, pinyin, 0, 1000)
}

/// 把「上一词」恢复成 `高`。
///
/// 真实输入法里引擎在每次提交后调用 `set_lm_context`，把**刚上屏的词**设为
/// 下一次查询的上文。测试直接调它来构造前提，等价于连续输入后的状态。
fn context_is_gao(d: &mut Dict) {
    d.set_lm_context(Some(("高", "gao")));
}

#[test]
fn realtime_bigram_uses_previous_word_as_key() {
    let db = tmp_db("zhzh");
    let mut d = Dict::open(&db).unwrap();

    // 高 → 频 5 次，高 → 果 1 次。
    for _ in 0..5 {
        d.log_commit(None, &["gao".into()], "高", None);
        d.log_commit(Some(("高", "gao")), &["pin".into()], "频", None);
    }
    d.log_commit(None, &["gao".into()], "高", None);
    d.log_commit(Some(("高", "gao")), &["guo".into()], "果", None);

    // 上一词 = 高：realtime_bigram["高"]["pin"] = 5、["guo"] = 1。
    context_is_gao(&mut d);
    let b_pre = d.lm_boost(&zh("频", "pin"));
    let b_guo = d.lm_boost(&zh("果", "guo"));

    assert!(b_pre > 0, "高→频 应给频非零加成，实际 {b_pre}");
    assert!(
        b_pre > b_guo,
        "频（5 次）应高于果（1 次）：{b_pre} vs {b_guo}"
    );
    assert!(
        b_pre <= CONTEXT_BOOST_CAP,
        "加成不得超过封顶 {CONTEXT_BOOST_CAP}"
    );
}

#[test]
fn realtime_bigram_requires_matching_previous_word() {
    let db = tmp_db("key");
    let mut d = Dict::open(&db).unwrap();

    d.log_commit(None, &["gao".into()], "高", None);
    d.log_commit(Some(("高", "gao")), &["pin".into()], "频", None);

    // 上一词是别的词 → 拿不到 高→频 的加成。
    d.set_lm_context(Some(("别", "bie")));
    assert_eq!(d.lm_boost(&zh("频", "pin")), 0);
}

#[test]
fn english_commit_does_not_pollute_zh_bigram() {
    let db = tmp_db("en");
    let mut d = Dict::open(&db).unwrap();

    // 英文提交（非 CJK）不建立中文 bigram。
    d.log_commit(None, &["if".into()], "if", None);
    d.log_commit(Some(("if", "if")), &["gao".into()], "高", None);

    context_is_gao(&mut d);
    assert_eq!(
        d.lm_boost(&zh("频", "pin")),
        0,
        "英文上下文不应污染中文 bigram"
    );
}

#[test]
fn lm_boost_clamped_to_cap() {
    let db = tmp_db("cap");
    let mut d = Dict::open(&db).unwrap();

    // 重复提交同一对，计数远超封顶所需。
    for _ in 0..10_000 {
        d.log_commit(None, &["gao".into()], "高", None);
        d.log_commit(Some(("高", "gao")), &["pin".into()], "频", None);
    }

    context_is_gao(&mut d);
    let b = d.lm_boost(&zh("频", "pin"));
    assert!(b > 0, "应有非零加成");
    assert!(
        b <= CONTEXT_BOOST_CAP,
        "加成 {b} 超过封顶 {CONTEXT_BOOST_CAP}"
    );
}

#[test]
fn context_recall_boosts_same_text_candidate() {
    let db = tmp_db("recall");
    let mut d = Dict::open(&db).unwrap();

    // 反复提交「频」，让上下文召回记住它。
    for _ in 0..5 {
        d.log_commit(None, &["pin".into()], "频", None);
    }

    // lookup_prefix 内部刷新片段缓存并写入「当前输入读音」，与引擎调用路径一致。
    let _ = d.lookup_prefix(&["pin".into()], "", 50);
    assert!(
        d.lm_boost(&zh("频", "pin")) > 0,
        "读音匹配时上下文召回应加成"
    );

    // 读音不匹配 → 召回不命中。
    let _ = d.lookup_prefix(&["hao".into()], "", 50);
    assert_eq!(d.lm_boost(&zh("频", "pin")), 0, "读音不匹配时不应加成");
}

#[test]
fn fragment_mined_in_stats() {
    let db = tmp_db("frag");
    let mut d = Dict::open(&db).unwrap();

    // 连续提交同一片段 3 次（达到 FRAGMENT_ADMISSION）。
    for _ in 0..3 {
        d.log_commit(None, &["xiang".into(), "mu".into()], "项目", None);
        d.log_commit(
            Some(("项目", "xiang'mu")),
            &["jin".into(), "du".into()],
            "进度",
            None,
        );
    }

    let stats = lm::mine_gated(d.conn(), None).unwrap();
    assert!(
        stats.fragments_mined > 0,
        "mine_gated 应报告 fragments_mined > 0，实际 {}",
        stats.fragments_mined
    );
}
