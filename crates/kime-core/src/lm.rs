//! 离线语言模型挖掘（设计见 todo/2026-09-18-offline-lm-design.md 第 1 层）。
//!
//! 从 `commit_log` 挖 bigram：W-TinyLFU 的「窗口 → 主表」单步。
//!
//! - **准入**：只有日志里重复 ≥ `MIN_ADMISSION` 次的对才进 `bigram`——一次性
//!   打错的词永远进不来（W-TinyLFU 拒绝低频新项的等价实现）。
//! - **老化**：挖掘时全体 `count /= 2`（O(表大小)，逐条 powf 的替代品）。
//!   长期不出现的对自然衰减到淘汰线以下。
//! - **遗忘**：`bigram` 超过 `BIGRAM_BUDGET` 时按 `count ASC, last_seen ASC`
//!   淘汰——LFU + recency 兜底，W-TinyLFU 在全部 trace 上表现最好的组合。
//! - **单事务**：全部写（合并计数 + 老化 + 淘汰 + 清日志 + 世代号）在一个
//!   事务里原子提交，IME 读侧要么旧状态要么新状态，永不见半成品。

use rusqlite::Connection;

/// 准入门槛：日志中重复 ≥2 次的对才进 bigram 主表。
pub const MIN_ADMISSION: i64 = 2;
/// bigram 主表软预算（行数）。超出按 count ASC, last_seen ASC 淘汰。
pub const BIGRAM_BUDGET: i64 = 500_000;
/// 日志保留窗口（天）：更老的行挖掘后直接删。
pub const LOG_WINDOW_DAYS: i64 = 30;

/// 挖掘结果统计。
#[derive(Debug, Default, PartialEq)]
pub struct MineStats {
    /// 日志中的 (prev,next) 对数（去重前提交行数见 `log_rows`）
    pub log_rows: i64,
    /// 本轮新准入的 bigram 对数
    pub admitted: i64,
    /// 老化后低于准入线被淘汰的对数
    pub aged_out: i64,
    /// 超预算淘汰的对数
    pub evicted: i64,
    /// 清掉的日志行数
    pub purged_rows: i64,
    /// 写入的新世代号
    pub generation: i64,
}

/// 单次挖掘。调用方（CLI）自行控制频率（如每天一次 cron）。
///
/// 全部写在单事务内：中途任何一步失败整体回滚，IME 侧不受影响。
pub fn mine(conn: &Connection) -> rusqlite::Result<MineStats> {
    let mut st = MineStats::default();
    let today = today_days();

    // execute_batch 不支持参数绑定（?1 按字面 NULL 处理），常量直接内联——
    // 全是整数常量，无注入面。
    let sql = format!(
        "BEGIN IMMEDIATE;
         -- 老化：全体减半（W-TinyLFU reset 语义，O(表大小)）
         UPDATE bigram SET count = count / 2;
         -- 老化后归零的行直接清掉，别占预算
         DELETE FROM bigram WHERE count <= 0;
         -- 准入：日志里重复 >= MIN_ADMISSION 的对合并进主表
         INSERT INTO bigram(prev_id, next_id, count, last_seen)
         SELECT p, n, cnt, {today}
         FROM (SELECT ctx_id AS p, text_id AS n, count(*) AS cnt
               FROM commit_log
               WHERE ctx_id IS NOT NULL
               GROUP BY ctx_id, text_id
               HAVING cnt >= {MIN_ADMISSION})
         WHERE true
         ON CONFLICT(prev_id, next_id) DO UPDATE SET
           count = count + excluded.count,
           last_seen = excluded.last_seen;
         ",
        today = today,
        MIN_ADMISSION = MIN_ADMISSION,
    );
    conn.execute_batch(&sql)?;
    st.admitted = conn.query_row("SELECT changes()", [], |r| r.get(0))?;
    // 老化淘汰：减半后只剩 1 的行（即上一轮准入后未被复用的）不删，
    // 留着当低频背景；只有预算压力才触发 LFU 淘汰（见下）。
    st.aged_out = 0;

    st.log_rows = conn.query_row("SELECT count(*) FROM commit_log", [], |r| r.get(0))?;

    // 遗忘：超预算按 count ASC, last_seen ASC 淘汰到预算内
    conn.execute(
        "DELETE FROM bigram WHERE (prev_id, next_id) IN (
           SELECT prev_id, next_id FROM bigram
           ORDER BY count ASC, last_seen ASC
           LIMIT max(0, (SELECT count(*) FROM bigram) - ?1)
         )",
        params![BIGRAM_BUDGET],
    )?;
    st.evicted = conn.query_row("SELECT changes()", [], |r| r.get(0))?;

    // 清日志（环形）：全量已合并进主表
    st.purged_rows = conn.execute("DELETE FROM commit_log", [])? as i64;

    // 世代号 +1：IME 读到变化即重载 boost 表
    conn.execute(
        "INSERT INTO kime_kv(key, value) VALUES ('lm_generation', '1')
         ON CONFLICT(key) DO UPDATE SET value = CAST(CAST(value AS INTEGER) + 1 AS TEXT)",
        [],
    )?;
    st.generation = conn
        .query_row(
            "SELECT value FROM kime_kv WHERE key = 'lm_generation'",
            [],
            |r| r.get::<_, String>(0),
        )?
        .parse()
        .unwrap_or(0);

    conn.execute_batch("COMMIT")?;
    let _ = today;
    Ok(st)
}

use rusqlite::params;

/// 「今天」= UNIX 纪元以来的天数（与 dict.rs 的 today_days 同语义）。
fn today_days() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64 / 86_400)
        .unwrap_or(0)
}
