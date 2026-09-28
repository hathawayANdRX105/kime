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

use crate::llm::LlmClient;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// 准入门槛：日志中重复 ≥2 次的对才进 bigram 主表。
pub const MIN_ADMISSION: i64 = 2;
/// jev 门控置信度阈值（bigram 准入）：语义判定达标线。
pub const JEV_BIGRAM_THRESHOLD: f64 = 0.6;
/// jev 门控置信度阈值（自动组词）：词库污染代价远高于漏学，比 bigram 更严。
pub const JEV_PHRASE_THRESHOLD: f64 = 0.7;
/// jev 判定批大小：准入候选对按 200 个一批分片，一批一个 HTTP 调用
///（离线一次性成本可接受；端点失败时该批整体降级纯计数）。
pub const JEV_BATCH: usize = 200;
/// jev 调用超时（秒）：离线挖掘不在热路径，容忍慢判定。
pub const JEV_TIMEOUT_SECS: u64 = 30;
/// jev 响应 max_tokens（最多 JEV_BATCH 个置信数的 JSON 数组）。
pub const JEV_MAX_TOKENS: u32 = 1024;
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
    /// 被 jev 门控拒绝的对数（置信度低于阈值，未准入；bigram 与组词两档合计）
    pub jev_gated: i64,
    /// 因 jev 端点失败/超时而降级回退纯计数的对数（降级路径不失败挖掘）
    pub jev_skipped: i64,
    /// 写入的新世代号
    pub generation: i64,
}

/// 单次挖掘（无 jev 门控 = 纯计数，行为与 #88 合入前一致）。
/// 调用方（CLI）自行控制频率（如每天一次 cron）。
///
/// 全部写在单事务内：中途任何一步失败整体回滚，IME 侧不受影响。
pub fn mine(conn: &Connection) -> rusqlite::Result<MineStats> {
    mine_gated(conn, None)
}

/// 同 `mine`，但 `gate` 给出时准入候选对（ctx 路径 + tail_ctx 切分路径，含
/// 自动组词候选）先送 jev 端点批量语义判定，置信度达标的才准入：
/// bigram 用 `JEV_BIGRAM_THRESHOLD`（0.6），组词用更严的 `JEV_PHRASE_THRESHOLD`
/// （0.7，词库污染代价高）。端点失败/超时 → 整批降级纯计数准入
///（`MineStats.jev_skipped`，一次性 eprintln 哨兵），降级路径永不失败挖掘。
/// `gate = None` 时行为与无门控逐字节一致。
pub fn mine_gated(conn: &Connection, gate: Option<&Jeving>) -> rusqlite::Result<MineStats> {
    let mut st = MineStats::default();
    let today = today_days();
    let mut warned = false;

    // jev 门控预扫（只读，事务前）：tail 挖掘计划 + 两路径准入候选对。
    let tail = plan_tail_pairs(conn)?;
    let bigram_accepted: Option<HashSet<(String, String)>> = match gate {
        Some(g) => {
            let (cands, c_samples) = gate_candidates(conn, &tail)?;
            let mut accepted: HashSet<(String, String)> = HashSet::new();
            for chunk in cands.chunks(JEV_BATCH) {
                let batch: Vec<(String, String)> = chunk.to_vec();
                let batch_samples: Vec<String> = batch
                    .iter()
                    .map(|p| c_samples.get(p).cloned().unwrap_or_default())
                    .collect();
                accepted.extend(gate_batch(
                    g,
                    &batch,
                    &batch_samples,
                    JEV_BIGRAM_THRESHOLD,
                    &mut st,
                    &mut warned,
                ));
            }
            Some(accepted)
        }
        None => None,
    };

    conn.execute_batch("BEGIN IMMEDIATE")?;
    // 老化：全体减半（W-TinyLFU reset 语义，O(表大小)）；归零行直接清掉
    conn.execute_batch(
        "UPDATE bigram SET count = count / 2;
         DELETE FROM bigram WHERE count <= 0;",
    )?;
    // 准入：日志里重复 >= MIN_ADMISSION 的对合并进主表；有门控时只作用于
    // jev 准入集合（文本空间 gate_pairs 临时表）——被拒对的日志行保留，
    // 下轮带更多证据再判（防双重计数不变式不变：只有合并了的行才删）。
    if let Some(accepted) = &bigram_accepted {
        conn.execute_batch("CREATE TEMP TABLE gate_pairs (pt TEXT NOT NULL, nt TEXT NOT NULL)")?;
        for (pt, nt) in accepted {
            conn.execute("INSERT INTO gate_pairs VALUES (?1, ?2)", params![pt, nt])?;
        }
        let sql = format!(
            "INSERT INTO bigram(prev_id, next_id, count, last_seen)
             SELECT p, n, cnt, {today}
             FROM (SELECT ctx_id AS p, text_id AS n, count(*) AS cnt
                   FROM commit_log
                   WHERE ctx_id IS NOT NULL
                   GROUP BY ctx_id, text_id
                   HAVING cnt >= {MIN_ADMISSION}) t
             JOIN vocab vp ON vp.id = t.p
             JOIN vocab vn ON vn.id = t.n
             WHERE EXISTS (SELECT 1 FROM gate_pairs g
                           WHERE g.pt = vp.text AND g.nt = vn.text)
             ON CONFLICT(prev_id, next_id) DO UPDATE SET
               count = count + excluded.count,
               last_seen = excluded.last_seen;",
            today = today,
            MIN_ADMISSION = MIN_ADMISSION,
        );
        conn.execute_batch(&sql)?;
    } else {
        // execute_batch 不支持参数绑定（?1 按字面 NULL 处理），常量直接内联——
        // 全是整数常量，无注入面。
        let sql = format!(
            "INSERT INTO bigram(prev_id, next_id, count, last_seen)
             SELECT p, n, cnt, {today}
             FROM (SELECT ctx_id AS p, text_id AS n, count(*) AS cnt
                   FROM commit_log
                   WHERE ctx_id IS NOT NULL
                   GROUP BY ctx_id, text_id
                   HAVING cnt >= {MIN_ADMISSION})
             WHERE true
             ON CONFLICT(prev_id, next_id) DO UPDATE SET
               count = count + excluded.count,
               last_seen = excluded.last_seen;",
            today = today,
            MIN_ADMISSION = MIN_ADMISSION,
        );
        conn.execute_batch(&sql)?;
    }
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
    // 带尾的行稍后在 apply_tail_plan 里按 rowid 处理（合并即删、未达标保留）。
    // 有门控时准入/清日志都只作用于 jev 接受集合：被拒对的行留下轮再判。
    if bigram_accepted.is_some() {
        let sql = format!(
            "DELETE FROM commit_log WHERE ctx_id IS NULL AND (tail_ctx IS NULL OR tail_ctx = '')
                 OR (ctx_id IS NOT NULL AND (ctx_id, text_id) IN (
                   SELECT ctx_id, text_id FROM commit_log
                   WHERE ctx_id IS NOT NULL
                   GROUP BY ctx_id, text_id
                   HAVING count(*) >= {MIN_ADMISSION}
                 )
                 AND EXISTS (
                   SELECT 1 FROM gate_pairs g
                   JOIN vocab vp ON vp.id = commit_log.ctx_id
                   JOIN vocab vn ON vn.id = commit_log.text_id
                   WHERE g.pt = vp.text AND g.nt = vn.text
                 ))",
            MIN_ADMISSION = MIN_ADMISSION,
        );
        st.purged_rows = conn.execute(&sql, [])? as i64;
    } else {
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
    }
    // tail 路径（#89）：tail_ctx 按词库前缀切分出前词，补真实相邻对。
    // 放在 SQL 清日志之后：带尾行按 rowid 删已合并的，未达标的保留供下轮
    // 累积（防双重计数语义与 ctx 路径一致）。有门控时合并集合再按 jev
    // 接受集合过滤。
    let (tail_mined, tail_purged) = apply_tail_plan(conn, today, &tail, bigram_accepted.as_ref())?;
    st.tail_mined = tail_mined;
    st.purged_rows += tail_purged;
    // 主事务提交：bigram 计数/淘汰/清日志原子生效
    conn.execute_batch("COMMIT")?;

    // 自动组词：反复相邻提交的对（≥ PHRASE_ADMISSION 次）learn 成用户词。
    // 用户打「项目」+「进度」若干次后，下次打 xiang'mu'jin'du 整串直接出
    // 「项目进度」——替用户完成组词（设计文档第 5 步）。
    // 独立执行：组词失败不影响已提交的 bigram 主数据。
    // 有门控时先按更严阈值（JEV_PHRASE_THRESHOLD）批量判定，只学接受集合。
    if let Some(g) = gate {
        let cands = phrase_candidates(conn)?;
        let mut accepted_phrases: HashSet<(String, String)> = HashSet::new();
        for chunk in cands.chunks(JEV_BATCH) {
            let batch: Vec<(String, String)> = chunk.to_vec();
            let samples = vec![String::new(); batch.len()];
            accepted_phrases.extend(gate_batch(
                g,
                &batch,
                &samples,
                JEV_PHRASE_THRESHOLD,
                &mut st,
                &mut warned,
            ));
        }
        st.phrases_learned = mine_phrases(conn, Some(&accepted_phrases)).unwrap_or(0);
    } else {
        st.phrases_learned = mine_phrases(conn, None).unwrap_or(0);
    }

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

/// 带尾挖掘计划（只读预扫，事务前）：`tail_ctx` 行按词库前缀切分出前词，
/// 派生 (前词, 本词) 相邻对并计数。前词留**文本空间**（vocab id 要
/// 事务内建，见 `apply_tail_plan`）；门控预扫与事务内合并共用本计划。
///
/// 切分算法见 `split_tail_by_dict`（#89，不动）；`pair_counts` = 派生对出现
/// 次数，`next_texts` = 本词 vocab 行文本（门控过滤 + prompt 用）。
struct TailPlan {
    /// 每行派生出的对（rowid, Option<(前词, 本词 vocab id)>）；
    /// None = 切分不可行，该行不参与挖掘
    row_pairs: Vec<(i64, Option<(String, i64)>)>,
    /// 派生对出现次数
    pair_counts: HashMap<(String, i64), i64>,
    /// 本词 vocab id → 文本
    next_texts: HashMap<i64, String>,
}

fn plan_tail_pairs(conn: &Connection) -> rusqlite::Result<TailPlan> {
    // (rowid, 本词 vocab id, 上屏前文尾, 本词文本)
    let mut stmt = conn.prepare(
        "SELECT c.rowid, c.text_id, c.tail_ctx, v.text
         FROM commit_log c
         JOIN vocab v ON v.id = c.text_id
         WHERE c.tail_ctx IS NOT NULL AND c.tail_ctx != ''",
    )?;
    let rows: Vec<(i64, i64, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .flatten()
        .collect();
    let mut plan = TailPlan {
        row_pairs: Vec::with_capacity(rows.len()),
        pair_counts: HashMap::new(),
        next_texts: HashMap::new(),
    };
    if rows.is_empty() {
        return Ok(plan);
    }

    let mut word_freq: HashMap<String, Option<i64>> = HashMap::new();
    for (rowid, text_id, tail, next_text) in &rows {
        plan.next_texts
            .entry(*text_id)
            .or_insert_with(|| next_text.clone());
        let pair = match split_tail_by_dict(conn, tail, &mut word_freq)? {
            Some(words) => {
                let Some(last) = words.last() else {
                    plan.row_pairs.push((*rowid, None));
                    continue;
                };
                Some((last.clone(), *text_id))
            }
            None => None,
        };
        if let Some(p) = &pair {
            *plan.pair_counts.entry(p.clone()).or_default() += 1;
        }
        plan.row_pairs.push((*rowid, pair));
    }
    Ok(plan)
}

/// 带尾挖掘应用（事务内，#89 的合并/删行语义）：达标对合并进 bigram 主表。
///
/// 返回 (达标对数, 删掉行数)。被删行 = 派生对已合并的行（按 rowid）：
/// 合并后留行下轮会二次累加同一相邻对（双重计数），必须删；未达标的
/// 行保留供下轮累积。ctx_id 是否为 NULL 不影响删除判定——带尾行的
/// 贡献以 tail 对为准，无尾行不在此路径（mine 的 SQL 已兜底 C2 不变式）。
///
/// `accepted` = jev 门控接受集合（文本空间 (前词, 本词文本)）；Some 时只有
/// 接受集合内的对才合并/删行（被拒对的行留下轮带更多证据再判），None =
/// 无门控（行为与 #88 合入前一致）。
fn apply_tail_plan(
    conn: &Connection,
    today: i64,
    plan: &TailPlan,
    accepted: Option<&HashSet<(String, String)>>,
) -> rusqlite::Result<(i64, i64)> {
    if plan.row_pairs.is_empty() {
        return Ok((0, 0));
    }

    // 前词 vocab id（必要时建 vocab 行——事务内写）；同一词只建一次
    let mut prev_ids: HashMap<String, i64> = HashMap::new();
    // 达标对合并进主表（与 ctx 路径同语义的 UPSERT）
    let mut merged_texts: HashSet<(String, String)> = HashSet::new();
    for ((word, next_id), count) in plan
        .pair_counts
        .iter()
        .filter(|(_, c)| **c >= MIN_ADMISSION)
    {
        let next_text = plan.next_texts.get(next_id).cloned().unwrap_or_default();
        let key = (word.clone(), next_text.clone());
        if let Some(acc) = accepted {
            if !acc.contains(&key) {
                continue;
            }
        }
        let prev_id = match prev_ids.get(word.as_str()) {
            Some(id) => *id,
            None => {
                let id = vocab_id_for_text(conn, word, today)?;
                prev_ids.insert(word.clone(), id);
                id
            }
        };
        conn.execute(
            "INSERT INTO bigram(prev_id, next_id, count, last_seen)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(prev_id, next_id) DO UPDATE SET
               count = count + excluded.count,
               last_seen = excluded.last_seen",
            params![prev_id, next_id, count, today],
        )?;
        merged_texts.insert(key);
    }
    // 派生对已合并的行按 rowid 删掉（防下轮双重计数）
    let mut purged = 0i64;
    for (rowid, pair) in &plan.row_pairs {
        if let Some((word, next_id)) = pair {
            let next_text = plan.next_texts.get(next_id).cloned().unwrap_or_default();
            if merged_texts.contains(&(word.clone(), next_text)) {
                conn.execute("DELETE FROM commit_log WHERE rowid = ?1", params![rowid])?;
                purged += 1;
            }
        }
    }
    Ok((merged_texts.len() as i64, purged))
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
/// `accepted` = jev 门控接受集合（文本空间）；Some 时只有接受集合内的对
/// 才学（组词走更严的 `JEV_PHRASE_THRESHOLD`），None = 无门控
///（行为与 #88 合入前一致）。
fn mine_phrases(
    conn: &Connection,
    accepted: Option<&HashSet<(String, String)>>,
) -> rusqlite::Result<i64> {
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
        // jev 门控：组词准入需更严阈值，只学接受集合内的对
        if let Some(acc) = accepted {
            if !acc.contains(&(p_text.to_string(), n_text.to_string())) {
                continue;
            }
        }
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

// =====================================================================
// jev 语义门控（#88）：准入候选先过语义判定再学库
// =====================================================================

/// jev 语义门控（离线挖掘侧）：准入候选对（ctx 路径 + tail_ctx 切分路径）
/// 批量送 jev 端点（OpenAI 兼容 /v1/chat/completions，协议与 `LlmClient`
/// 同源，不新增 HTTP 客户端）判定「作为中文 IME 相邻输入是自然语言组合
/// 吗」，置信度达标才准入。端点失败/超时/解析失败 → 整批降级纯计数
///（降级路径永不失败挖掘），一次性 eprintln 哨兵。
#[derive(Clone)]
pub struct Jeving {
    client: LlmClient,
}

impl Jeving {
    /// `api_key` 只从 CLI `--jev-key` 或 KIME_JEV_KEY 环境变量传入——
    /// 凭据不入 config 明文、不入词库、不入任何提交文件。
    pub fn new(endpoint: String, model: String, api_key: Option<String>) -> Self {
        Self {
            client: LlmClient::new_with_timeout(
                endpoint,
                model,
                Duration::from_secs(JEV_TIMEOUT_SECS),
            )
            .with_api_key(api_key),
        }
    }

    /// 判定批：返回与 `pairs` 对齐的置信度表（0..1，每对一个）；
    /// 端点失败/超时/内容不可解析/个数不齐 → `None`（调用方整批降级纯计数）。
    /// `samples` = 与 `pairs` 对齐的上下文样本行（可为空串 = 无样本）。
    fn score_batch(&self, pairs: &[(String, String)], samples: &[String]) -> Option<Vec<f64>> {
        let prompt = build_gate_prompt(pairs, samples);
        let content = self.client.chat_raw_sync(&prompt, JEV_MAX_TOKENS).ok()?;
        let (Some(start), Some(end)) = (content.find('['), content.rfind(']')) else {
            return None;
        };
        if start >= end {
            return None;
        }
        let scores: Vec<f64> = serde_json::from_str(&content[start..=end]).ok()?;
        if scores.len() != pairs.len() {
            return None;
        }
        Some(scores)
    }
}

/// 纯函数 seam：按逐对置信度表判定准入（无端点可单测）。
/// `scores` 与 `pairs` 对齐（等长，`score_batch` 契约保证）；返回同长标志表，
/// 对 i 准入 ⟺ scores[i] >= threshold。阈值由调用方给两档：
/// bigram 用 `JEV_BIGRAM_THRESHOLD`（0.6），组词用 `JEV_PHRASE_THRESHOLD`（0.7）。
/// 缺分数的批是降级路径（回退纯计数），不该调用本函数。
pub fn gate_pairs(pairs: &[(String, String)], scores: &[f64], threshold: f64) -> Vec<bool> {
    debug_assert_eq!(pairs.len(), scores.len(), "scores 与 pairs 对齐");
    scores.iter().map(|s| *s >= threshold).collect()
}

/// 跑一批门控：返回本批「准入对」= 门控接受对 ∪ 降级整批。
/// 拒计数落 `st.jev_gated`、降级计数落 `st.jev_skipped`；降级哨兵
/// eprintln 每次挖掘只打一次（`warned`）。
fn gate_batch(
    gate: &Jeving,
    pairs: &[(String, String)],
    samples: &[String],
    threshold: f64,
    st: &mut MineStats,
    warned: &mut bool,
) -> Vec<(String, String)> {
    match gate.score_batch(pairs, samples) {
        Some(scores) => {
            let flags = gate_pairs(pairs, &scores, threshold);
            st.jev_gated += flags.iter().filter(|f| !**f).count() as i64;
            pairs
                .iter()
                .zip(flags.iter())
                .filter_map(|(p, f)| f.then(|| p.clone()))
                .collect()
        }
        None => {
            if !*warned {
                eprintln!("[kime] jev 门控不可用（端点失败/超时），本次挖掘降级为纯计数准入");
                *warned = true;
            }
            st.jev_skipped += pairs.len() as i64;
            pairs.to_vec()
        }
    }
}

/// 拼 noul 判定 prompt：逐对问「作为中文 IME 相邻输入是自然语言组合吗」，
/// 附上下文样本；输出契约 = 逐对置信度 JSON 数组。
fn build_gate_prompt(pairs: &[(String, String)], samples: &[String]) -> String {
    let mut s = String::from(
        "请逐对判断：以下每一对，作为中文 IME 的相邻输入（用户依次键入的两个相邻词），是否构成自然语言组合：\n",
    );
    for (i, (pair, sample)) in pairs.iter().zip(samples.iter()).enumerate() {
        let (prev, next) = pair;
        s.push_str(&format!("{}. 「{prev}」+「{next}」", i + 1));
        if !sample.is_empty() {
            s.push_str(&format!("（上下文样本：{sample}）"));
        }
        s.push('\n');
    }
    s.push_str(
        "只输出一个 JSON 数组：每个对一项置信度（0..1），按顺序对齐，不要任何其它字符。\n\
         示例：[0.9, 0.1, 0.75]",
    );
    s
}

/// jev 门控预扫：两路径准入候选对（计数 ≥ `MIN_ADMISSION`，文本空间去重）
/// + 每对一条上下文样本（空串 = 无样本）。只读，事务前调用。
fn gate_candidates(
    conn: &Connection,
    tail: &TailPlan,
) -> rusqlite::Result<(Vec<(String, String)>, HashMap<(String, String), String>)> {
    // ctx 路径：commit_log 里重复 >= MIN_ADMISSION 的相邻对 + 上屏前文尾样本
    let sql = format!(
        "SELECT pv.text, nv.text, max(cl.tail_ctx)
         FROM commit_log cl
         JOIN vocab pv ON pv.id = cl.ctx_id
         JOIN vocab nv ON nv.id = cl.text_id
         WHERE cl.ctx_id IS NOT NULL
         GROUP BY cl.ctx_id, cl.text_id
         HAVING count(*) >= {MIN_ADMISSION}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .flatten()
        .collect::<Vec<_>>();

    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut samples: HashMap<(String, String), String> = HashMap::new();
    for (pt, nt, tail_ctx) in rows {
        let sample = tail_ctx
            .filter(|t| !t.is_empty())
            .map(|t| format!("…{t} {pt} {nt}"))
            .unwrap_or_else(|| format!("{pt} {nt}"));
        pairs.push((pt.clone(), nt.clone()));
        samples.entry((pt, nt)).or_insert(sample);
    }
    // tail 路径：派生对（计数 >= MIN_ADMISSION）
    for ((word, next_id), count) in &tail.pair_counts {
        if *count < MIN_ADMISSION {
            continue;
        }
        let next_text = tail.next_texts.get(next_id).cloned().unwrap_or_default();
        let key = (word.clone(), next_text.clone());
        pairs.push(key.clone());
        samples
            .entry(key)
            .or_insert_with(|| format!("{word} {next_text}"));
    }
    // 去重（同一对可能两条路径都有证据）
    let mut seen: HashSet<(String, String)> = HashSet::new();
    pairs.retain(|p| seen.insert(p.clone()));
    Ok((pairs, samples))
}

/// 自动组词候选（文本空间对）：与 `mine_phrases` 的 SELECT 同域
///（bigram 计数 ≥ PHRASE_ADMISSION、非空单段读音对、top 50），按文本对去重
/// ——门控预扫按更严阈值批量判定用。
fn phrase_candidates(conn: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    let sql = format!(
        "SELECT pv.text, nx.text
         FROM bigram b
         JOIN vocab pv ON pv.id = b.prev_id
         JOIN vocab nx ON nx.id = b.next_id
         WHERE b.count >= {PHRASE_ADMISSION}
           AND pv.reading != '' AND nx.reading != ''
           AND pv.reading NOT LIKE '% %' AND nx.reading NOT LIKE '% %'
         ORDER BY b.count DESC
         LIMIT 50",
        PHRASE_ADMISSION = PHRASE_ADMISSION,
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .flatten()
        .collect::<Vec<_>>();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for p in rows {
        if seen.insert(p.clone()) {
            out.push(p);
        }
    }
    Ok(out)
}
