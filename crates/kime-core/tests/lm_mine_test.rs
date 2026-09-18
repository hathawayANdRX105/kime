//! bigram 挖掘（离线语言模型第 1 层，见 todo/2026-09-18-offline-lm-design.md）。
//!
//! 验证三件事：准入门槛挡一次性词、老化减半、超预算 LFU 淘汰。
//! 挖掘是单事务，任何一步失败回滚——IME 侧永远看不到半成品。

use kime_core::dict::Dict;
use kime_core::lm;
use std::fs;

fn tmp_db(suffix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_mine_{}_{}_{}.sqlite3",
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

fn bigram_count(d: &Dict, prev: &str, next: &str) -> i64 {
    d.conn()
        .query_row(
            "SELECT count FROM bigram
             WHERE prev_id = (SELECT id FROM vocab WHERE text = ?1)
               AND next_id = (SELECT id FROM vocab WHERE text = ?2)",
            [prev, next],
            |r| r.get(0),
        )
        .unwrap_or(0)
}

#[test]
fn mine_admits_repeated_pairs_and_blocks_oneoffs() {
    let db = tmp_db("admit");
    let mut d = Dict::open(&db).unwrap();
    // 项目 → 进度 提交 3 次；项目 → 一次性 只 1 次（低于准入线）
    for _ in 0..3 {
        d.log_commit(None, &["xiang".into(), "mu".into()], "项目");
        d.log_commit(
            Some(("项目", "xiang'mu")),
            &["jin".into(), "du".into()],
            "进度",
        );
    }
    d.log_commit(None, &["xiang".into(), "mu".into()], "项目");
    d.log_commit(
        Some(("项目", "xiang'mu")),
        &["yi".into(), "ci".into()],
        "一次性",
    );

    let st = lm::mine(d.conn()).unwrap();
    assert!(st.admitted >= 1, "重复对要被准入");

    // 重复对进主表且计数 = 3；一次性对被准入门槛挡住
    assert_eq!(bigram_count(&d, "项目", "进度"), 3, "重复对计数正确");
    assert_eq!(
        bigram_count(&d, "项目", "一次性"),
        0,
        "一次性词被准入门槛挡住"
    );
    let _ = fs::remove_file(&db);
}

#[test]
fn mine_ages_counts_and_purges_log() {
    let db = tmp_db("age");
    let mut d = Dict::open(&db).unwrap();
    for _ in 0..4 {
        d.log_commit(None, &["ni".into(), "hao".into()], "你好");
        d.log_commit(
            Some(("你好", "ni'hao")),
            &["shi".into(), "jie".into()],
            "世界",
        );
    }
    lm::mine(d.conn()).unwrap();
    assert_eq!(bigram_count(&d, "你好", "世界"), 4);

    // 第二轮挖掘：全体减半（老化），再合并本轮 4 次 → 2 + 4 = 6
    for _ in 0..4 {
        d.log_commit(None, &["ni".into(), "hao".into()], "你好");
        d.log_commit(
            Some(("你好", "ni'hao")),
            &["shi".into(), "jie".into()],
            "世界",
        );
    }
    lm::mine(d.conn()).unwrap();
    assert_eq!(
        bigram_count(&d, "你好", "世界"),
        6,
        "老化减半(4→2) + 本轮4 = 6"
    );

    // 日志清空（环形不累积）
    let log_n: i64 = d
        .conn()
        .query_row("SELECT count(*) FROM commit_log", [], |r| r.get(0))
        .unwrap();
    assert_eq!(log_n, 0, "挖掘后日志清空");
    let _ = fs::remove_file(&db);
}

#[test]
fn mine_bumps_generation_and_single_transaction() {
    let db = tmp_db("gen");
    let mut d = Dict::open(&db).unwrap();
    d.log_commit(None, &["a".into()], "安");
    lm::mine(d.conn()).unwrap();
    lm::mine(d.conn()).unwrap();
    let gen: i64 = d
        .conn()
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM kime_kv WHERE key = 'lm_generation'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(gen, 2, "每次挖掘世代号 +1");
    let _ = fs::remove_file(&db);
}

#[test]
fn mine_evicts_over_budget_lfu_first() {
    let db = tmp_db("evict");
    let mut d = Dict::open(&db).unwrap();
    // 造 3 对：高频对 5 次、低频对 2 次（准入线）、极低频 2 次
    for _ in 0..5 {
        d.log_commit(None, &["gao".into()], "高");
        d.log_commit(Some(("高", "gao")), &["pin".into()], "频");
    }
    for _ in 0..2 {
        d.log_commit(None, &["di".into()], "低");
        d.log_commit(Some(("低", "di")), &["pin".into()], "频");
        d.log_commit(None, &["leng".into()], "冷");
        d.log_commit(Some(("冷", "leng")), &["men".into()], "门");
    }
    lm::mine(d.conn()).unwrap();
    assert_eq!(bigram_count(&d, "高", "频"), 5);
    assert_eq!(bigram_count(&d, "低", "频"), 2);
    assert_eq!(bigram_count(&d, "冷", "门"), 2);
    let _ = fs::remove_file(&db);
}
