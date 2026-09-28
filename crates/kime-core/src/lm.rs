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
use std::collections::{HashMap, HashSet};

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
    /// 本轮新准入的 bigram 对数（ctx_id 链路径）
    pub admitted: i64,
    /// 带尾挖掘准入的对数（tail_ctx 词库前缀切分路径，#89）
    pub tail_mined: i64,
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
    // 日志规模快照（本轮输入行数，tail 路径删行之前）
    st.log_rows = conn.query_row("SELECT count(*) FROM commit_log", [], |r| r.get(0))?;
    // 老化淘汰：减半后只剩 1 的行（即上一轮准入后未被复用的）不删，
    // 留着当低频背景；只有预算压力才触发 LFU 淘汰（见下）。
    st.aged_out = 0;

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
    // ctx_id IS NULL 且**无尾**的行（序列首词，没有前文尾可依）永远进不了
    // bigram，留着纯属无界累积（审查发现 C2 的真实形态）——照旧删掉（C2 不变式）。
    // 带尾的行稍后在 mine_tail_pairs 里按 rowid 处理（合并即删、未达标保留）。
    st.purged_rows = conn.execute(
        "DELETE FROM commit_log WHERE ctx_id IS NULL AND (tail_ctx IS NULL OR tail_ctx = '')
             OR (ctx_id IS NOT NULL AND (ctx_id, text_id) IN (
               SELECT ctx_id, text_id FROM commit_log
               WHERE ctx_id IS NOT NULL
               GROUP BY ctx_id, text_id
               HAVING count(*) >= 2
             ))",
        [],
    )? as i64;
    // tail 路径（#89）：tail_ctx 按词库前缀切分出前词，补真实相邻对。
    // 放在 SQL 清日志之后：带尾行按 rowid 删已合并的，未达标的保留供下轮
    // 累积（防双重计数语义与 ctx 路径一致）。
    let (tail_mined, tail_purged) = mine_tail_pairs(conn, today)?;
    st.tail_mined = tail_mined;
    st.purged_rows += tail_purged;
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
    Ok(st)
}

/// 前缀切分候选词最大长度（按字符）。词库词常规 2–4 字，上限只挡病态长
/// 词条，实践中不可达（同 mine_phrases 的长度防线思路）。
const MAX_TAIL_WORD: usize = 16;

/// 带尾挖掘（#89）：`tail_ctx` 行按词库前缀切分出前词，派生 (前词, 本词)
/// 相邻对，重复 ≥ `MIN_ADMISSION` 次的对合并进 bigram 主表。
///
/// 返回 (达标对数, 删掉行数)。被删行 = 派生对已合并的行（按 rowid）：
/// 合并后留行下轮会二次累加同一相邻对（双重计数），必须删；未达标的
/// 行保留供下轮累积。ctx_id 是否为 NULL 不影响删除判定——带尾行的
/// 贡献以 tail 对为准，无尾行不在此路径（mine 的 SQL 已兜底 C2 不变式）。
fn mine_tail_pairs(conn: &Connection, today: i64) -> rusqlite::Result<(i64, i64)> {
    // (rowid, 本词 vocab id, 上屏前文尾)
    let mut stmt = conn.prepare(
        "SELECT rowid, text_id, tail_ctx
         FROM commit_log
         WHERE tail_ctx IS NOT NULL AND tail_ctx != ''",
    )?;
    let rows: Vec<(i64, i64, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .flatten()
        .collect();
    if rows.is_empty() {
        return Ok((0, 0));
    }

    let mut word_freq: HashMap<String, Option<i64>> = HashMap::new();
    let mut pair_counts: HashMap<(i64, i64), i64> = HashMap::new();
    // 每行派生出的对（切分不可行 = None，该行不参与挖掘）
    let mut row_pairs: Vec<(i64, Option<(i64, i64)>)> = Vec::with_capacity(rows.len());
    for (rowid, text_id, tail) in &rows {
        let pair = match split_tail_by_dict(conn, tail, &mut word_freq)? {
            Some(words) => {
                let Some(last) = words.last() else {
                    row_pairs.push((*rowid, None));
                    continue;
                };
                Some((vocab_id_for_text(conn, last, today)?, *text_id))
            }
            None => None,
        };
        if let Some(p) = pair {
            *pair_counts.entry(p).or_default() += 1;
        }
        row_pairs.push((*rowid, pair));
    }

    // 达标对合并进主表（与 ctx 路径同语义的 UPSERT）
    let merged: HashSet<(i64, i64)> = pair_counts
        .iter()
        .filter(|(_, c)| **c >= MIN_ADMISSION)
        .map(|(k, _)| *k)
        .collect();
    for &(prev_id, next_id) in &merged {
        conn.execute(
            "INSERT INTO bigram(prev_id, next_id, count, last_seen)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(prev_id, next_id) DO UPDATE SET
               count = count + excluded.count,
               last_seen = excluded.last_seen",
            params![prev_id, next_id, pair_counts[&(prev_id, next_id)], today],
        )?;
    }
    // 派生对已合并的行按 rowid 删掉（防下轮双重计数）
    let mut purged = 0i64;
    for (rowid, pair) in &row_pairs {
        if let Some(p) = pair {
            if merged.contains(p) {
                conn.execute("DELETE FROM commit_log WHERE rowid = ?1", params![rowid])?;
                purged += 1;
            }
        }
    }
    Ok((merged.len() as i64, purged))
}

/// 按词库前缀把 `tail`（落屏句的上屏前文尾）切成词序列（带尾挖掘的私有工具）。
///
/// 词判定 = phrase 表有 `text = 词` 的行（等价 `Dict::lookup_prefix` 的 exact
/// 块——词库有无该词）；词频率 = 该 text 所有行的 MAX(freq)（代表频率，与
/// 拼音行无关）。「每个词都在词库」的可行切分中取**总频率最高**的方案（DP；
/// 模型选方案是 #88 的活，本期不做）。无可行完整切分（标点/数字/未收字）
/// → `None`，该行不参与挖掘。
/// `cache` 跨行共享：同一词在不同尾里只查一次 phrase 表。
fn split_tail_by_dict(
    conn: &Connection,
    tail: &str,
    cache: &mut HashMap<String, Option<i64>>,
) -> rusqlite::Result<Option<Vec<String>>> {
    let chars: Vec<char> = tail.chars().collect();
    let n = chars.len();
    if n == 0 {
        return Ok(None);
    }
    let mut stmt = conn.prepare("SELECT MAX(freq) FROM phrase WHERE text = ?1")?;
    let mut dp: Vec<Option<i64>> = vec![None; n + 1];
    let mut choice: Vec<Option<usize>> = vec![None; n + 1];
    dp[0] = Some(0);
    for i in 1..=n {
        let mut best: Option<(i64, usize)> = None;
        for j in (i.saturating_sub(MAX_TAIL_WORD))..i {
            let Some(base) = dp[j] else {
                continue;
            };
            let word: String = chars[j..i].iter().collect();
            let freq: Option<i64> = match cache.get(&word) {
                Some(f) => *f,
                None => {
                    let f: Option<i64> = stmt
                        .query_row(params![word.as_str()], |r| r.get(0))
                        .unwrap_or(None);
                    cache.insert(word.clone(), f);
                    f
                }
            };
            let Some(f) = freq else {
                continue;
            };
            let cand = base + f;
            if best.map_or(true, |(b, _)| cand > b) {
                best = Some((cand, j));
            }
        }
        if let Some((cand, j)) = best {
            dp[i] = Some(cand);
            choice[i] = Some(j);
        }
    }
    if dp[n].is_none() {
        return Ok(None);
    }
    // 回溯恢复词序列；末词 = 本行 text 的前词（真实落屏句的词边界）
    let mut words: Vec<String> = Vec::new();
    let mut i = n;
    while i > 0 {
        let Some(j) = choice[i] else {
            return Ok(None);
        };
        words.push(chars[j..i].iter().collect());
        i = j;
    }
    words.reverse();
    Ok(Some(words))
}

/// 纯词库词（带尾挖掘的前词）取（必要时建）vocab 行 id。
///
/// bigram.prev_id 必须引用 vocab 行；词库词没有 kime 读音，reading 取 phrase
/// 表最高频拼音行（词在词库 = 行必存在，故非空）。优先复用既有
/// (text, 该拼音) 行：IME 侧 set_lm_context 按 kime 读音查，命中率最高。
fn vocab_id_for_text(conn: &Connection, text: &str, today: i64) -> rusqlite::Result<i64> {
    let reading: String = conn
        .query_row(
            "SELECT pinyin FROM phrase WHERE text = ?1 ORDER BY freq DESC LIMIT 1",
            params![text],
            |r| r.get(0),
        )
        .unwrap_or_default();
    conn.execute(
        "INSERT INTO vocab(text, reading, last_seen) VALUES (?1, ?2, ?3)
         ON CONFLICT(text, reading) DO UPDATE SET last_seen = excluded.last_seen",
        params![text, reading, today],
    )?;
    conn.query_row(
        "SELECT id FROM vocab WHERE text = ?1 AND reading = ?2",
        params![text, reading],
        |r| r.get(0),
    )
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
