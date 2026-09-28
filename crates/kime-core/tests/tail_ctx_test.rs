//! commit_log tail_ctx（#89）：上屏时引擎把落屏句的上屏前文尾（context）
//! 落进 `commit_log.tail_ctx`，离线挖掘按词库前缀把它切成前词，补出
//! (前词, 本词) 真实相邻对。
//!
//! 钉四件事（red 条件见 todo/handoff/tail-ctx-89.md §4）：
//! - 切分取真实相邻对（漏前词 → 红）
//! - 多可行切分取总频最高方案（取首方案 → 红）
//! - 无尾旧行维持 ctx_id 链行为（旧数据被新路径误删 → 红）
//! - ctx_id NULL 且有尾的行参与挖掘（被 C2 删除 → 红）
//! 外加：既有库（无 tail_ctx 列）开库自动 ALTER 补列不崩（#89 验收 4）。

use kime_core::dict::Dict;
use kime_core::lm;
use std::fs;

fn tmp_db(suffix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_tailctx_{}_{}_{}.sqlite3",
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

/// 造一条 bigram（与 `bigram` 主表同口径：按 vocab 文本查计数）。
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

/// 造一行词库（phrase 表）：freq 决定前缀切分 DP 的加权。
fn seed_phrase(d: &Dict, pinyin: &str, text: &str, freq: i64) {
    d.conn()
        .execute(
            "INSERT INTO phrase(pinyin, text, freq, abbrev, user)
             VALUES (?1, ?2, ?3, ?4, 0)",
            rusqlite::params![pinyin, text, freq, ""],
        )
        .unwrap();
}

#[test]
fn tail_ctx_mines_adjacent() {
    let db = tmp_db("adjacent");
    let mut d = Dict::open(&db).unwrap();
    seed_phrase(&d, "jin tian", "今天", 5000);
    seed_phrase(&d, "xiang'mu", "项目", 5000);
    seed_phrase(&d, "jin'du", "进度", 4000);
    // 前文 "今天项目" + 上屏 "进度"（ctx 为空：前词只能靠尾切分出）
    for _ in 0..2 {
        d.log_commit(None, &["jin".into(), "du".into()], "进度", Some("今天项目"));
    }
    let st = lm::mine(d.conn()).unwrap();
    // 切分 [今天, 项目] 的末词 = 项目 → bigram (项目, 进度)；
    // 若切分漏了前词（末词取成 今天），这里 0 → 红。
    assert_eq!(
        bigram_count(&d, "项目", "进度"),
        2,
        "末词 = 前词（切分正确）"
    );
    assert_eq!(st.tail_mined, 1, "带尾挖掘对数统计");
    // 已合并的带尾行被清掉（下轮不重复计数）
    let remaining: i64 = d
        .conn()
        .query_row("SELECT count(*) FROM commit_log", [], |r| r.get(0))
        .unwrap();
    assert_eq!(remaining, 0, "已合并行清空");
    let _ = fs::remove_file(&db);
}

#[test]
fn tail_ctx_multi_way_scales_by_freq() {
    let db = tmp_db("multiway");
    let mut d = Dict::open(&db).unwrap();
    // 前文 "天项目" 两种可行切分：
    //   [天项目]        总频 1
    //   [天, 项目]      总频 1 + 5000 = 5001 ← 应取（末词 = 项目）
    seed_phrase(&d, "tian xiang mu", "天项目", 1);
    seed_phrase(&d, "tian", "天", 1);
    seed_phrase(&d, "xiang'mu", "项目", 5000);
    seed_phrase(&d, "jin'du", "进度", 4000);
    for _ in 0..2 {
        d.log_commit(None, &["jin".into(), "du".into()], "进度", Some("天项目"));
    }
    let st = lm::mine(d.conn()).unwrap();
    // 取首方案（[天项目]）→ 前词变「天项目」，这里 0 → 红
    assert_eq!(
        bigram_count(&d, "项目", "进度"),
        2,
        "多方案取总频最高（末词 = 项目）"
    );
    assert_eq!(bigram_count(&d, "天项目", "进度"), 0, "低频方案不应入选");
    assert_eq!(st.tail_mined, 1);
    let _ = fs::remove_file(&db);
}

#[test]
fn legacy_rows_keep_ctx_chain() {
    let db = tmp_db("legacy");
    let mut d = Dict::open(&db).unwrap();
    // 旧形态：无尾，纯 ctx 链（你好 → 世界 重复 2 次）
    for _ in 0..2 {
        d.log_commit(None, &["ni".into(), "hao".into()], "你好", None);
        d.log_commit(
            Some(("你好", "ni'hao")),
            &["shi".into(), "jie".into()],
            "世界",
            None,
        );
    }
    let st = lm::mine(d.conn()).unwrap();
    // 现状行为不变：ctx 对走旧 SQL 准入；带尾挖掘数为 0
    assert_eq!(
        bigram_count(&d, "你好", "世界"),
        2,
        "无尾旧行维持 ctx_id 链行为"
    );
    assert_eq!(st.tail_mined, 0, "无尾行不进 tail 路径");
    // C2 不变式保留：无尾的 NULL ctx 行照旧删（旧数据没被新路径误留/误删）
    let remaining: i64 = d
        .conn()
        .query_row("SELECT count(*) FROM commit_log", [], |r| r.get(0))
        .unwrap();
    assert_eq!(remaining, 0, "旧清日志规则不变");
    let _ = fs::remove_file(&db);
}

#[test]
fn null_ctx_with_tail_participates() {
    let db = tmp_db("nullctx");
    let mut d = Dict::open(&db).unwrap();
    seed_phrase(&d, "xiang'mu", "项目", 5000);
    seed_phrase(&d, "jin'du", "进度", 4000);
    // 所有行 ctx 都 NULL：旧语义下会被 C2 直接删掉、永无挖掘机会；
    // 有尾之后应先挖再清。
    for _ in 0..2 {
        d.log_commit(None, &["jin".into(), "du".into()], "进度", Some("项目"));
    }
    let st = lm::mine(d.conn()).unwrap();
    assert_eq!(
        bigram_count(&d, "项目", "进度"),
        2,
        "NULL ctx + 有尾的行参与挖掘（未被 C2 吞掉）"
    );
    assert_eq!(st.purged_rows, 2, "合并后带尾行清掉");
    let _ = fs::remove_file(&db);
}

#[test]
fn legacy_db_without_tail_ctx_column_migrates() {
    let db = tmp_db("migrate");
    {
        // 手工造「旧库」：commit_log 没有 tail_ctx 列（#89 之前的 schema）
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE commit_log (
               ts      INTEGER NOT NULL,
               ctx_id  INTEGER,
               text_id INTEGER NOT NULL,
               reading TEXT    NOT NULL
             );
             INSERT INTO commit_log(ts, ctx_id, text_id, reading)
             VALUES (1, NULL, 1, 'ni')",
        )
        .unwrap();
    }
    // 开库不崩 + 自动补列（既有库迁移路径）
    let mut d = Dict::open(&db).unwrap();
    let has_col: bool = d
        .conn()
        .prepare("PRAGMA table_info(commit_log)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .map(|cols| cols.flatten().any(|n| n == "tail_ctx"))
        .unwrap_or(false);
    assert!(has_col, "旧库缺列时 Dict::open 用 ALTER 补上");
    // 旧行可读，tail_ctx = NULL
    let tail: Option<String> = d
        .conn()
        .query_row("SELECT tail_ctx FROM commit_log LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tail, None, "旧行无值 = NULL");
    // 迁移后照常可写带尾提交
    d.log_commit(None, &["jin".into(), "du".into()], "进度", Some("项目"));
    let tail2: Option<String> = d
        .conn()
        .query_row(
            "SELECT tail_ctx FROM commit_log ORDER BY rowid DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tail2, Some("项目".into()), "迁移后带尾提交照常落列");
    let _ = fs::remove_file(&db);
}
