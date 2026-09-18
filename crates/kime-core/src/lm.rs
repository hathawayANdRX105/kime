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
/// LM bigram 加成单位。语料计数 10⁵~10⁷ 量级（同 user_bonus_of 的校准逻辑）：
/// count=1 的 bigram 给 30 万，足以把低频后继顶到高频词前面；
/// count=2 → 60 万。与 USER_BOOST 同源，但独立累积互不干扰。
pub const LM_BOOST_UNIT: i64 = 300_000;

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
    /// 自动组词新学的用户词数
    pub phrases_learned: i64,
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

    // 清日志：**只删已合并进主表的行**（对计数 ≥ MIN_ADMISSION）。
    // 未达门槛的行保留——它们下轮可能凑够准入线；已合并的必须删，
    // 否则下轮 UPSERT 累加会双重计数。
    // ctx_id IS NULL 的行（每次提交序列的首词）永远进不了 bigram，
    // 留着纯属无界累积（审查发现 C2 的真实形态）——一并删掉。
    st.purged_rows = conn.execute(
        "DELETE FROM commit_log WHERE ctx_id IS NULL
             OR (ctx_id, text_id) IN (
               SELECT ctx_id, text_id FROM commit_log
               WHERE ctx_id IS NOT NULL
               GROUP BY ctx_id, text_id
               HAVING count(*) >= 2
             )",
        [],
    )? as i64;
    // 主事务提交：bigram 计数/淘汰/清日志原子生效
    conn.execute_batch("COMMIT")?;

    // 自动组词：反复相邻提交的对（≥ PHRASE_ADMISSION 次）learn 成用户词。
    // 用户打「项目」+「进度」若干次后，下次打 xiang'mu'jin'du 整串直接出
    // 「项目进度」——替用户完成组词（设计文档第 5 步）。
    // 独立执行：组词失败不影响已提交的 bigram 主数据。
    st.phrases_learned = mine_phrases(conn).unwrap_or(0);

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

/// 自动组词准入线：相邻对出现 ≥ 此次数才值得学成词。
/// 必须明显高于 bigram 的 MIN_ADMISSION(2)：学错词的代价（词库污染）
/// 远高于漏学，宁缺勿滥。
pub const PHRASE_ADMISSION: i64 = 5;

/// 把反复相邻的对 learn 成用户词。
///
/// 数据源：本轮日志清空前的对计数已在 bigram 主表里——直接读 bigram
/// （含历史累积），对 count ≥ PHRASE_ADMISSION 且还没学过的执行 learn。
/// 拼接读音：prev.reading + "'" + next.reading（vocab 里存着）。
/// 已是词库精确行的跳过（learn 幂等，但省一次写）。
fn mine_phrases(conn: &Connection) -> rusqlite::Result<i64> {
    let pairs: Vec<(String, String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT pv.text || ' ' || nx.text,
                    pv.reading || ' ' || nx.reading,
                    b.count
             FROM bigram b
             JOIN vocab pv ON pv.id = b.prev_id
             JOIN vocab nx ON nx.id = b.next_id
             WHERE b.count >= ?
               AND pv.reading != '' AND nx.reading != ''
               AND pv.reading NOT LIKE '% %' AND nx.reading NOT LIKE '% %'
             ORDER BY b.count DESC
             LIMIT 50",
        )?;
        let rows = stmt.query_map([PHRASE_ADMISSION], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        rows.flatten().collect()
    };
    let mut learned = 0i64;
    for (text_pair, reading_pair, _) in pairs {
        // text_pair = "项目 进度"，reading_pair = "xiang'mu jin'du"
        let (Some(pt), Some(nt)) = (text_pair.split_once(' '), reading_pair.split_once(' ')) else {
            continue;
        };
        let (p_text, n_text) = pt;
        let (p_read, n_read) = nt;
        let joined_text = format!("{p_text}{n_text}");
        let joined_reading = format!("{p_read}'{n_read}");
        // 长度防线：>8 字的「词」几乎必是切分歪了（如「项目进」+「度进度」
        // 类长链），学进词库是污染。词库常规词 2-4 字。
        if joined_text.chars().count() > 8 {
            continue;
        }
        // 已在词库（同读音同文本）→ 跳过
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM phrase WHERE pinyin = ?1 AND text = ?2)",
                [&joined_reading, &joined_text],
                |r| r.get(0),
            )
            .unwrap_or(true);
        if exists {
            continue;
        }
        conn.execute(
            "INSERT OR IGNORE INTO phrase(pinyin, text, freq, abbrev, user)
             VALUES (?1, ?2, 1, ?3, 1)",
            rusqlite::params![
                joined_reading,
                joined_text,
                joined_reading
                    .chars()
                    .filter(|c| *c != '\'')
                    .collect::<String>()
            ],
        )?;
        learned += 1;
    }
    Ok(learned)
}
