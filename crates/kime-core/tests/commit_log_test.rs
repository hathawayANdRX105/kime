//! commit_log 数据采集（离线语言模型第 0 层，见
//! todo/2026-09-18-offline-lm-design.md）。
//!
//! log_commit 是离线挖掘唯一的原材料入口：写丢了，bigram 就无米下锅。
//! 这里同时验证「重开库后日志仍在」——挖掘工具是独立进程，必须能读到
//! IME 进程写下的行。

use std::fs;

use kime_core::dict::Dict;

fn tmp_db(suffix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_commit_{}_{}_{}.sqlite3",
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

#[test]
fn log_commit_chains_context_and_normalizes_vocab() {
    let db = tmp_db("chain");
    {
        let mut d = Dict::open(&db).unwrap();
        // 连续两次提交：你好 → 世界。第二次带「上文」。
        d.log_commit(None, &["ni".into(), "hao".into()], "你好");
        d.log_commit(
            Some(("你好", "ni'hao")),
            &["shi".into(), "jie".into()],
            "世界",
        );
    }
    // 重开库：挖掘工具是独立进程，必须看到 IME 写下的行。
    let d = Dict::open(&db).unwrap();
    let rows: Vec<(Option<i64>, i64)> = d
        .conn()
        .prepare("SELECT ctx_id, text_id FROM commit_log ORDER BY rowid")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(rows.len(), 2, "两次提交都要落日志");
    assert!(rows[0].0.is_none(), "首次提交无上文");
    let hello_id = rows[0].1;
    assert_eq!(rows[1].0, Some(hello_id), "第二次的上文 = 第一次的 text_id");
    assert_ne!(rows[1].1, hello_id, "世界 与 你好 应是不同 vocab 行");
    let vocab_n: i64 = d
        .conn()
        .query_row("SELECT count(*) FROM vocab", [], |r| r.get(0))
        .unwrap();
    assert_eq!(vocab_n, 2, "vocab 归一化：两词各存一次");
    let _ = fs::remove_file(&db);
}

#[test]
fn log_commit_ignores_empty_input() {
    let db = tmp_db("empty");
    let mut d = Dict::open(&db).unwrap();
    d.log_commit(None, &[], "你好");
    d.log_commit(None, &["ni".into()], "");
    let n: i64 = d
        .conn()
        .query_row("SELECT count(*) FROM commit_log", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "空读音或空文本不该落日志");
    let _ = fs::remove_file(&db);
}
