//! M1: SQLite 持久层（rusqlite bundled，单文件库）。
//!
//! schema 契约：
//!
//! ```sql
//! CREATE TABLE phrase (
//!   pinyin  TEXT    NOT NULL,  -- 音节 `'` 连接："ni'hao"
//!   text    TEXT    NOT NULL,  -- 上屏文本
//!   freq    INTEGER NOT NULL DEFAULT 0,
//!   abbrev  TEXT    NOT NULL,  -- 声母缩写："nh"
//!   user    INTEGER NOT NULL DEFAULT 0  -- 0=词库(导入只增) 1=用户词(学习写)
//! );
//! -- 索引：(pinyin)、(abbrev)、(text) —— 查询恒为 index scan + freq 排序 LIMIT n；
//! --   (text) 服务上下文反查 [`Dict::readings_of_text`]，老库 open 时一次性补建
//! --   （92 万行约数百 ms，发生在用户打第一下之前，可接受）。
//! -- 英文词另立一表（`english(text PRIMARY KEY, freq)`），见 `import_english`：
//! -- 它们不是拼音，进 `phrase` 会污染前缀查询。
//! ```
//!
//! 主路延迟（P99 < 20ms）由这里的索引形状保证。

use rusqlite::{params, Connection, Error as SqliteError, OptionalExtension, Result};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::learn_writer::{LearnJob, LearnWriter};

fn io_to_sqlite(e: std::io::Error) -> SqliteError {
    SqliteError::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
}
/// 用户词提频：算式、常量与键类型都归 [`crate::learn`]（中英文共用一套语义）。
///
/// 这里只留一个薄封装，让既有调用点保持 `(stats, today, pinyin, text)` 形状；
/// 加成永不写回 `freq`，只落排序用 `eff`。
fn user_bonus_of(stats: &crate::learn::UserStats, today: u64, pinyin: &str, text: &str) -> u64 {
    stats.bonus_of(today, pinyin, text)
}

/// 「今天」= UNIX 纪元以来的天数。衰减窗口按天推进即可（半衰期本身 30 天粒度）。
///
/// 放在持久层是因为开库烘焙与学习当帧生效都按它算衰减；`learn_writer` 的后台写
/// 线程也用它给落盘计数盖日期。
pub(crate) fn today_days() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0)
}

/// 烘焙进条目的有效频率：user 行 = 裸频 + 提频加成（每行一次 powf，开库/learn 时算），
/// 语料行 = 裸频。加成只落 `eff` 字段：`Candidate.freq`/`phrase.freq` 保持原始值
/// （「词库频率+次数」），`user_overlay_test`(5001) / `top_user_works`(5002) /
/// `memory_vs_sql_consistency` 钉的就是它。比较器因此是纯字段比较——热路径零哈希零 powf。
fn entry_eff_freq(e: &IndexEntry) -> u64 {
    e.eff
}

/// (pinyin ASC, 有效频率 DESC, text ASC) —— 与旧裸频率序的唯一差异在 user 行的 eff。
fn entry_cmp(a: &IndexEntry, b: &IndexEntry) -> std::cmp::Ordering {
    a.pinyin
        .cmp(&b.pinyin)
        .then_with(|| b.eff.cmp(&a.eff))
        .then_with(|| a.text.cmp(&b.text))
}
/// 候选词 —— 全链路统一货币（定义见 [`crate::model::Candidate`]）。
///
/// 再导出而非重复定义：`kime_core::dict::Candidate` 是历史公开路径，平台壳与测试
/// 都按这个路径引用。模型本身（语言/层/来源三个字段）属于候选模型层，不属于持久层。
pub use crate::model::{Candidate, Lang, Layer, SourceKind};

/// 内存排序索引条目，按 (pinyin ASC, freq DESC, text ASC) 排序；
/// `eff` = 排序用有效频率（开库/learn 烘焙，比较器纯字段比较，热路径零哈希零 powf）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// 排序键：拼音升序
    pub pinyin: String,
    /// 显示文本
    pub text: String,
    /// 词频（用户词 freq+1 或新词插入）
    pub freq: i64,
    /// 声母缩写，用于 abbrev 前缀查询
    pub abbrev: String,
    /// 用户词标记（0=词库，1=用户词）
    pub user: i64,
    /// 排序用有效频率（user=1 时 = 裸频 + 提频加成，否则 = 裸频）
    pub eff: u64,
}

/// 英文词条目。`key` = `text` 的小写形式（text 恒为纯 ASCII 字母，见 `import_english`）；
/// 整个 Vec 按 (key ASC, freq DESC, text ASC) 排序，`lookup_english` 的前缀区间靠它二分。
///
/// `eff` = 排序用有效频率（裸频 + 用户提频加成），开库时烘焙、学习时重烘——
/// 与中文 `IndexEntry.eff` 同一套做法：加成不进比较器，热路径零哈希零分配。
struct EnglishEntry {
    key: String,
    text: String,
    freq: i64,
    eff: u64,
}

pub struct Dict {
    conn: Connection,
    /// 中文表内最大有效频率（跨语言分数归一化的基准）。
    ///
    /// 见 [`crate::rank::ScoreScale`]：为什么必须校准、为什么用对数压缩。
    /// 导入/学习会抬高它，所以是字段而不是开库时算死的常量。
    zh_max: u64,
    /// 英文表内最大有效频率（同上）。
    en_max: u64,
    /// 内存排序索引：按 (pinyin ASC, freq DESC, text ASC) 排序
    index: Vec<IndexEntry>,
    /// 二级索引：同一批条目按 (abbrev ASC, freq DESC, text ASC) 排序——abbrev 前缀二分用
    abbrev_index: Vec<IndexEntry>,
    /// 可选的 FST 二进制词库存储
    store: Option<crate::store::FstStore>,
    /// FST 模式下的用户词 overlay，按 pinyin ASC 排序。
    ///
    /// 用户词是极小集合（个位到几十条），但 `user = 1` 的 SQL 谓词无索引可用：
    /// 每次按键都得靠 `idx_phrase_pinyin` 定位再逐行过滤 192 万行里的 user 列，
    /// 实测单键 10–57ms。全量常驻内存后热路径只做二分。
    user_overlay: Vec<IndexEntry>,
    /// 语料总词频（`SUM(freq)`）。整句联想要把词频换算成概率才可比。
    ///
    /// 在开库时算一次：92 万行无索引全扫约 150ms，但那会儿用户还没开始打字。
    /// 首次组句时懒算试过，结果这 150ms 正好砸在第一下按键上（实测 `zuiqi` 一键 52–120ms）。
    total_freq: u64,
    /// 英文词全量常驻内存：rime-ice 两表去重后 21,708 条、约 2 MB。开库时一次性读出来
    /// （见 `load_english`，实测多花 28ms），之后每次按键只做二分，绝不再碰 SQLite。
    english: Vec<EnglishEntry>,
    /// 用户学习统计（中英文共用一套计数与加成算式，见 [`crate::learn`]）。
    ///
    /// 语料计数与用户计数分开的「独立空间」：phrase.freq 不动，排序加成由
    /// (n, 日期) 现算。持久化在无 schema 迁移的 `kime_kv(key, value)` 表——
    /// 选它而不是把计数压进 freq 高位：后者会污染所有读 freq 的路径
    /// （builder、overlay 权威读回、memory_vs_sql 一致性）。
    ///
    /// 键是带语言标签的 [`crate::learn::LearnKey`]：中文 = (读音, 文本)，
    /// 英文 = (词, 词)。语言是键类型的一部分，不再靠「读音是否等于文本」这类
    /// 内容判据区分——旧实现正是用那条判据把英文计数挡在中文重排之外。
    user_stats: crate::learn::UserStats,
    /// 有提频记录的中文读音集合（user_stats 中文键的读音投影）。查询时 O(1) 判断
    /// 「该拼音块是否可能含提频行」——只有打学到过的拼音才触发块内 eff 重排；
    /// 英文计数永不进这个集合（`LearnKey::En` 不参与投影）。
    user_pinyins: std::collections::HashSet<String>,
    /// `today_days()` 在开库/learn 时刷新的缓存值（比较器热路径不碰系统时钟）。
    today: u64,
    /// 词表由本进程自己写入，进程内无外部并发写入者，缓存不会脏。
    vocab_ids: HashMap<(String, String), i64>,
    /// LM 上下文：上次提交词的 vocab id（None = 无上下文，不加成）。
    lm_ctx: Option<i64>,
    /// lm_ctx 的后继 bigram 计数，两级键 text → pinyin → count：
    /// cand_cmp 每次比较都要查（O(n log n) 次），嵌套结构用 &str 两跳
    /// 查找，避免扁平 (String,String) 键的每比较两次 String 分配。
    lm_counts: HashMap<String, HashMap<String, i64>>,
    /// 已消费的挖掘世代号（kime_kv 'lm_generation'）：变化即重载 lm_counts。
    lm_generation: i64,
    /// commit_log 写失败已告警哨兵（一次性告警，防每键刷屏；见 `warn_once`）。
    lm_warned: bool,
    /// 数据库路径：后台写线程懒 spawn 时按它开自己的连接（#101，
    /// 见 `learn_writer` 模块）。写线程不存副本，单份存储在这里。
    db_path: PathBuf,
    /// 后台词库写线程句柄（懒 spawn；队满丢 job，键路径零 SQLite 写）。
    writer: LearnWriter,
    /// 轻量上下文召回：最近 kime 提交过的片段/句子及其读音，LRU + hits，上限 64。
    context_recall: ContextRecall,
    /// 当前输入读音（transient）。lookup 前由引擎设置，供 lm_boost 的上下文召回匹配用。
    /// 单线程所有，无并发顾虑。未设置时召回加成为 0。
    current_input_reading: RefCell<Option<String>>,
    /// 离线片段缓存：由 refresh_fragment_cache 填充，供 lm_boost 的片段召回加成用。
    /// 引擎在 lookup 前按当前读音刷新；未刷新时片段加成为 0。
    fragment_cache: RefCell<Vec<crate::lm::FragmentCandidate>>,
    /// 实时 bigram：前词文本 → 后继读音 → 计数。仅 commit_context_sync 写入，
    /// 绝不被 set_lm_context 清空，与离线 lm_counts 互补。
    realtime_bigram: HashMap<String, HashMap<String, i64>>,
    /// 上一次上屏词（实时 bigram 的外键）。`commit_context_sync` 写入本次词，
    /// `set_lm_context` 按传入的 prev 覆盖；`lm_boost` 用它做「上一词 → 候选」查表。
    ///
    /// 没有它，实时 bigram 只能按「候选自己的文本」查——而 bigram 的语义是
    /// 「上一词的后继里出现过这个读音」，按候选文本查等于永远查不到（历史 bug）。
    lm_prev_text: Option<String>,
    /// shell surrounding_text（光标前上文，汉字文本）。仅作召回线索：
    /// 候选文本出现在这段上文里时给加成，用来恢复「刚打过的词」。
    /// 与 `context_recall` 分开，因为后者按**读音**匹配，汉字永不命中。
    context_tail: Option<String>,
}

/// 打开 rime `.dict.yaml`，交回 `...` 分隔线**之后**的正文行。中文/英文两条导入路径共用。
fn dict_body_lines(path: &Path) -> Result<impl Iterator<Item = String>> {
    let f = File::open(path).map_err(io_to_sqlite)?;
    let mut lines = BufReader::new(f).lines().map_while(Result::ok);
    for line in &mut lines {
        if line.trim() == "..." {
            break;
        }
    }
    Ok(lines)
}

/// 声母缩写（"ni'hao" → "nh"）。learn 的内存更新与持久化载荷共用，算式只有一份。
fn abbrev_of(reading: &[String]) -> String {
    let abbrev: String = reading.iter().filter_map(|s| s.chars().next()).collect();
    abbrev.to_lowercase()
}

/// 一次性告警：flag 置位后静默，防每键刷屏；写 stderr 失败直接吞——
/// 输入法与后台写线程都不许用会在 stderr 关闭时 panic 的打印宏。
pub(crate) fn warn_once(flag: &mut bool, message: &str) {
    if *flag {
        return;
    }
    *flag = true;
    let _ = writeln!(std::io::stderr(), "{message}");
}

/// 挖掘世代号（kime_kv 'lm_generation'）。行不存在/查询失败按 0，与挖掘前状态一致。
fn lm_generation_of(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COALESCE(CAST(value AS INTEGER), 0) FROM kime_kv
         WHERE key = 'lm_generation'",
        [],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

/// 世代号失效：挖掘跑过 → vocab id 缓存全部作废重查（否则重建后的 vocab 用了
/// 陈旧 id，commit_log 会记到错误的词上）。选词路径（`set_lm_context`）与后台
/// 写线程共用这一份实现。
pub(crate) fn refresh_vocab_cache(
    conn: &Connection,
    generation: &mut i64,
    vocab_ids: &mut HashMap<(String, String), i64>,
) {
    let gen = lm_generation_of(conn);
    if gen != *generation {
        *generation = gen;
        vocab_ids.clear();
    }
}

/// 取（必要时建）vocab 行 id —— commit_log 落库的前置，同步 `log_commit` 与
/// 后台写线程共用。`log_commit` 每次提交都调，命中缓存时零 SQL。
///
/// SELECT 命中 → 缓存；未命中 → 插入（UNIQUE 冲突说明并发已建，忽略）再回查拿 id。
/// 不使用 RETURNING：旧 SQLite 不支持，且 execute 不返回行。读写失败一律 None——
/// 跳过本次日志，不阻塞上屏。
fn vocab_id_or_insert(
    conn: &Connection,
    vocab_ids: &mut HashMap<(String, String), i64>,
    text: &str,
    reading: &str,
    today: u64,
) -> Option<i64> {
    let key = (text.to_string(), reading.to_string());
    if let Some(&id) = vocab_ids.get(&key) {
        return Some(id);
    }
    let id = conn
        .query_row(
            "SELECT id FROM vocab WHERE text = ?1 AND reading = ?2",
            params![text, reading],
            |row| row.get(0),
        )
        .ok()
        .or_else(|| {
            let _ = conn.execute(
                "INSERT OR IGNORE INTO vocab(text, reading, last_seen) VALUES (?1, ?2, ?3)",
                params![text, reading, today as i64],
            );
            conn.query_row(
                "SELECT id FROM vocab WHERE text = ?1 AND reading = ?2",
                params![text, reading],
                |row| row.get(0),
            )
            .ok()
        });
    if let Some(id) = id {
        vocab_ids.insert(key, id);
    }
    id
}

/// 取 vocab 行 id（只读，#101）：`set_lm_context` 在选词同步路径上，miss 绝不回写
/// ——键路径零 SQLite 写。miss → None → 本次跳过 LM 上下文，vocab 行落盘后下一次
/// 选词自愈。命中的 id 照样进缓存（id 建立后永不变，世代号变化统一清）。
fn vocab_id_readonly(
    conn: &Connection,
    vocab_ids: &mut HashMap<(String, String), i64>,
    text: &str,
    reading: &str,
) -> Option<i64> {
    let key = (text.to_string(), reading.to_string());
    if let Some(&id) = vocab_ids.get(&key) {
        return Some(id);
    }
    let id: i64 = conn
        .query_row(
            "SELECT id FROM vocab WHERE text = ?1 AND reading = ?2",
            params![text, reading],
            |row| row.get(0),
        )
        .ok()?;
    vocab_ids.insert(key, id);
    Some(id)
}

/// learn 持久化：phrase 提权 + kime_kv 使用计数。同步 [`Dict::learn`] 与后台
/// 写线程共用这一份 SQL（#101：不许复制两份）。
///
/// - phrase：单条 UPSERT——行存在 → `freq+1, user=1`（旧 UPDATE 分支语义），
///   不存在 → `freq=1` 插入（旧 INSERT 分支语义），语义等价且少一次往返、
///   少一个竞态窗口。
/// - kime_kv：写**绝对值** `n,day`（由 learn_mem 算好传入），队列丢一个 job
///   后下一个 job 仍收敛到最新计数。
pub(crate) fn persist_learn(
    conn: &Connection,
    pinyin: &str,
    abbrev: &str,
    text: &str,
    n: u64,
    day: u64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO phrase(pinyin, text, freq, abbrev, user) VALUES (?1, ?2, 1, ?3, 1)
         ON CONFLICT(pinyin, text) DO UPDATE SET freq = freq + 1, user = 1",
        params![pinyin, text, abbrev],
    )?;
    conn.execute(
        "INSERT INTO kime_kv(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![format!("{pinyin}\t{text}"), format!("{n},{day}")],
    )?;
    Ok(())
}

/// 英文词使用计数落库：**只**写 `kime_kv` 旁表，不碰 `phrase`。
///
/// - `phrase` 是拼音前缀查询的数据源，混进英文词会污染中文候选（打 `he` 捞出 `help`），
///   所以英文学习只落旁表。
/// - key = `词\t词`，value = `n,day`（绝对值）：与中文学习同格式，开库时同一条
///   [`Dict::load_user_stats`] 读回，加成算式完全共用。
pub(crate) fn persist_english_learn(conn: &Connection, word: &str, n: u64, day: u64) -> Result<()> {
    conn.execute(
        "INSERT INTO kime_kv(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![format!("{word}\t{word}"), format!("{n},{day}")],
    )?;
    Ok(())
}

/// commit_log 落库（离线 LM 第 0 层）。同步 [`Dict::log_commit`] 与后台写线程
/// 共用（#101：SQL 只有这一份）。
///
/// vocab id 在**执行时**解析（调用方传入自己的缓存；后台写线程按世代号失效，
/// 见 [`refresh_vocab_cache`]）。读音/文本为空直接 no-op，与旧 `log_commit`
/// 入口守卫同语义；INSERT 失败向上抛，由调用方一次性告警。
pub(crate) fn persist_commit_log(
    conn: &Connection,
    vocab_ids: &mut HashMap<(String, String), i64>,
    ctx: Option<(&str, &str)>,
    reading: &str,
    text: &str,
    tail: Option<&str>,
    today: u64,
) -> Result<()> {
    if reading.is_empty() || text.is_empty() {
        return Ok(());
    }
    let Some(text_id) = vocab_id_or_insert(conn, vocab_ids, text, reading, today) else {
        return Ok(());
    };
    let ctx_id = ctx.and_then(|(t, r)| vocab_id_or_insert(conn, vocab_ids, t, r, today));
    conn.execute(
        "INSERT INTO commit_log(ts, ctx_id, text_id, reading, tail_ctx)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![today as i64, ctx_id, text_id, reading, tail],
    )?;
    Ok(())
}

/// 上下文加成上限：必须是 tie-breaker / 有限加成，不压过裸频。
/// USER_BOOST=300_000（首次使用提频），上下文加成取其一半=150_000：
/// 足以让等频/近频候选中「上下文相关」者排前，但不足以让低频历史词
/// 无脑压过高频语料词（150k < 300k < 500k 语料词）。
pub const CONTEXT_BOOST_CAP: i64 = 150_000;

/// 实时 bigram 的单位加成：`count × 该值`，封顶 [`CONTEXT_BOOST_CAP`]。
///
/// 取 30_000（= CAP / 5）是为了让**次数**可分辨：count=1 得 30k、
/// count=5 得满 150k，于是「用了五次的连接」确实排在「用过一次的连接」之前。
/// 若沿用 `lm::LM_BOOST_UNIT`（300_000），第一条证据就越过封顶，所有非零
/// 计数并列在 150k —— 计数信息全丢，实时 bigram 退化成「出现过没有」的布尔标记。
pub const CONTEXT_BIGRAM_UNIT: i64 = 30_000;

/// 上下文召回条目上限：最近 64 条片段，内存占用极小（约数 KB）。
/// 热路径在 `lm_boost` 内对条目做线性扫描（≤64 次 `&str` 比较，零分配），
/// 对每键排序的 O(n log n) 比较来说属于常数级开销。
pub const CONTEXT_RECALL_CAP: usize = 64;

/// 轻量上下文召回条目：一条最近 kime 提交过的片段/句子及其读音。
/// `hits` 记录被提交/召中的次数，用于给加成加权；被 CONTEXT_BOOST_CAP 封顶。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextEntry {
    pub text: String,
    /// 音节 `'` 连接（如 "gao'pin"）。
    pub reading: String,
    pub hits: u64,
}

/// 轻量上下文召回结构：记录最近 kime 提交过的片段/句子及其读音。
/// 同一文本+读音命中则刷新 hits 并前移；未命中则追加到头部，
/// 超过上限时丢弃尾部条目（LRU 风格）。
#[derive(Clone, Debug, Default)]
pub struct ContextRecall {
    pub entries: Vec<ContextEntry>,
}

impl ContextRecall {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// 记录一条已提交的片段/句子。同一文本+读音命中则刷新 hits 并前移；
    /// 未命中则追加到头部，超过上限时丢弃尾部条目（LRU 风格）。
    pub fn record(&mut self, text: &str, reading: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(pos) = self
            .entries
            .iter()
            .position(|e| e.text == text && e.reading == reading)
        {
            self.entries[pos].hits += 1;
            let entry = self.entries.remove(pos);
            self.entries.insert(0, entry);
            return;
        }
        if self.entries.len() >= CONTEXT_RECALL_CAP {
            self.entries.pop();
        }
        self.entries.insert(
            0,
            ContextEntry {
                text: text.to_string(),
                reading: reading.to_string(),
                hits: 1,
            },
        );
    }

    /// 为候选计算上下文召回加成。
    /// 当前输入读音与条目读音匹配（精确相等或前缀/后缀关系）且条目文本等于
    /// 候选文本时，给予与 hits 相关的加成，被 CONTEXT_BOOST_CAP 封顶。
    pub fn boost_for(
        &self,
        candidate: &crate::model::Candidate,
        current_reading: Option<&str>,
    ) -> i64 {
        let Some(current) = current_reading else {
            return 0;
        };
        let mut best_hits: u64 = 0;
        for entry in &self.entries {
            if entry.text != candidate.text {
                continue;
            }
            // 当前读音与条目读音：精确相等，或互为前缀
            if entry.reading == current
                || entry.reading.starts_with(current)
                || current.starts_with(entry.reading.as_str())
            {
                best_hits = best_hits.max(entry.hits);
            }
        }
        if best_hits > 0 {
            (best_hits as i64 * 50_000).clamp(0, CONTEXT_BOOST_CAP)
        } else {
            0
        }
    }
}
impl Dict {
    /// 只读连接引用：离线挖掘工具与集成测试查 commit_log/bigram 用。
    /// 不暴露 &mut：写路径必须走 Dict 的方法，保证内存索引与库一致。
    pub fn conn(&self) -> &rusqlite::Connection {
        &self.conn
    }
    fn dict_bin_path(db_path: &Path) -> PathBuf {
        db_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .join("dict.bin")
    }

    /// 打开；不存在则建 schema + 索引
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             -- WAL：离线挖掘工具批量写时 IME 并发读，零锁冲突。
             -- journal_mode 持久存于 DB header，已为 WAL 则本行 no-op；
             -- busy_timeout 不持久，必须每次连接设置（ms）。
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS phrase (
               pinyin  TEXT    NOT NULL,
               text    TEXT    NOT NULL,
               freq    INTEGER NOT NULL DEFAULT 0,
               abbrev  TEXT    NOT NULL,
               user    INTEGER NOT NULL DEFAULT 0,
               UNIQUE(pinyin, text)
             );
             CREATE INDEX IF NOT EXISTS idx_phrase_text ON phrase(text);
             -- 部分索引：只收 user = 1 的行（个位到几十条）。开库时载入用户词 overlay
             -- 靠它一次 seek 拿到，否则 `WHERE user = 1` 无索引可用 → 全表扫 192 万行（实测 2.6s）。
             CREATE INDEX IF NOT EXISTS idx_phrase_user ON phrase(pinyin) WHERE user = 1;
             -- 英文词：独立表，绝不与 phrase 混用（见 `import_english` 的注释）。
             -- text 即上屏文本，也是匹配键的来源（小写化后），故 PRIMARY KEY = text。
             CREATE TABLE IF NOT EXISTS english (
               text  TEXT    NOT NULL PRIMARY KEY,
               freq  INTEGER NOT NULL DEFAULT 0
             );
             -- 用户提频计数旁表（见 user_stats 字段注释）：key = pinyin + TAB + text，
             -- value = n,day（day = 纪元以来天数）。几十行小表，不建索引。
             CREATE TABLE IF NOT EXISTS kime_kv (
               key   TEXT    NOT NULL PRIMARY KEY,
               value TEXT    NOT NULL
             );
            CREATE TABLE IF NOT EXISTS commit_log (
              ts       INTEGER NOT NULL,
              ctx_id   INTEGER,
              text_id  INTEGER NOT NULL,
              reading  TEXT    NOT NULL,
              -- 落屏句的上屏前文尾（引擎 context，光标前上下文，见 engine.rs）：
              -- 离线挖掘按词库前缀把它切成前词，补出 (前词, 本词) 真实相邻对。
              tail_ctx TEXT
            );
             -- vocab = 词表：text/reading 只存一次，bigram 用整数 ID 引用
             -- （压缩 + 整数比较比文本排序快，见设计文档第四节）。
             CREATE TABLE IF NOT EXISTS vocab (
               id      INTEGER PRIMARY KEY,
               text    TEXT    NOT NULL,
               reading TEXT    NOT NULL,
               last_seen INTEGER NOT NULL DEFAULT 0,
               UNIQUE(text, reading)
             );
             -- bigram = 「主表」：仅收录在 commit_log 里重复 >= 2 次的对
             -- （W-TinyLFU 准入门槛，一次性打错的词进不来）。
             -- count 周期性全体减半实现老化；超预算按 count ASC, last_seen ASC 淘汰。
             CREATE TABLE IF NOT EXISTS bigram (
               prev_id   INTEGER NOT NULL,
               next_id   INTEGER NOT NULL,
               count     INTEGER NOT NULL DEFAULT 1,
               last_seen INTEGER NOT NULL DEFAULT 0,
               PRIMARY KEY(prev_id, next_id)
             );
             CREATE INDEX IF NOT EXISTS idx_bigram_prev ON bigram(prev_id);
             CREATE INDEX IF NOT EXISTS idx_bigram_last ON bigram(count, last_seen);
             -- 离线片段表：由 mine_fragments 挖掘，供在线片段召回加成用。
             -- UNIQUE(text, reading) 保证同一片段只存一行；count 累积出现次数。
             CREATE TABLE IF NOT EXISTS lm_fragment (
               id        INTEGER PRIMARY KEY,
               text      TEXT    NOT NULL,
               reading   TEXT    NOT NULL,
               count     INTEGER NOT NULL DEFAULT 1,
               last_seen INTEGER NOT NULL DEFAULT 0,
               UNIQUE(text, reading)
             );
             CREATE INDEX IF NOT EXISTS idx_lm_fragment_reading ON lm_fragment(reading);
             CREATE INDEX IF NOT EXISTS idx_lm_fragment_count ON lm_fragment(count, last_seen);
             ",
        )?;
        // 既有库缺列迁移（世代兼容）：旧 dict.sqlite3 的 commit_log 没有 tail_ctx，
        // CREATE TABLE IF NOT EXISTS 对既有表不补列，这里显式查列缺失则 ALTER；
        // 新建库已带列，整段 no-op。开库一次，无热路径开销。
        {
            let has_tail_ctx: bool = conn
                .prepare("PRAGMA table_info(commit_log)")?
                .query_map(params![], |row| row.get::<_, String>(1))
                .map(|cols| cols.flatten().any(|name| name == "tail_ctx"))
                .unwrap_or(false);
            if !has_tail_ctx {
                conn.execute_batch("ALTER TABLE commit_log ADD COLUMN tail_ctx TEXT")?;
            }
        }
        // 用户提频计数先加载：下面内存索引的排序按「有效频率」（裸 freq + 加成）走。
        let user_stats = Self::load_user_stats(&conn)?;
        let today = today_days();
        // Try to load FST binary if present alongside DB
        let bin_path = Self::dict_bin_path(path);
        let mut store: Option<crate::store::FstStore> = None;
        if bin_path.exists() {
            match crate::store::FstStore::open(&bin_path) {
                Ok(s) => {
                    store = Some(s);
                }
                Err(e) => {
                    let _ = writeln!(
                        std::io::stderr(),
                        "[kime] 警告：FST 词库 {} 加载失败 ({e})，回退到 SQLite 内存索引（启动更慢、更占内存）",
                        bin_path.display()
                    );
                }
            }
        }
        // If no FST or load failed, load existing index from SQLite.
        // SQL 的 ORDER BY 与 entry_cmp 的非用户行序完全一致；eff 在载入时逐行烘焙
        // （user 行一次 powf，共 ~百行），因此**不再整索引重排**——旧代码在已序结果上
        // 再跑 192 万行 × O(n log n) 次比较，纯浪费。boost 只影响 user 行在其拼音块内的
        // 位置，查询时按 user_pinyins 条件重排（见 lookup/lookup_prefix）。
        let mut index: Vec<IndexEntry> = Vec::new();
        if store.is_none() {
            let mut stmt = conn.prepare(
                "SELECT pinyin, text, freq, abbrev, user FROM phrase ORDER BY pinyin ASC, freq DESC, text ASC",
            )?;
            let rows = stmt.query_map(params![], |row| {
                Ok(IndexEntry {
                    pinyin: row.get(0)?,
                    text: row.get(1)?,
                    freq: row.get(2)?,
                    abbrev: row.get(3)?,
                    user: row.get(4)?,
                    eff: 0,
                })
            })?;
            index = rows.collect::<Result<Vec<_>, _>>()?;
            for e in &mut index {
                if e.user == 1 {
                    e.eff = e.freq.max(0) as u64
                        + user_bonus_of(&user_stats, today, &e.pinyin, &e.text);
                } else {
                    e.eff = e.freq.max(0) as u64;
                }
            }
        }
        let mut abbrev_index = index.clone();
        abbrev_index.sort_by(|a, b| {
            a.abbrev
                .cmp(&b.abbrev)
                .then_with(|| b.eff.cmp(&a.eff))
                .then_with(|| a.text.cmp(&b.text))
        });
        // 有提频记录的中文读音集合：查询时判断「该块是否需要按 eff 重排」的 O(1)
        // 依据（集合是个位到几百条，层一块内重排只在打学到过的拼音时发生）。
        //
        // 语言过滤在这里是**类型自带**的：`UserStats` 的键是带语言标签的
        // `LearnKey`，`zh_pinyins()` 只投影中文键。旧实现靠「两段相等 ⇒ 英文」
        // 的内容判据过滤，一旦英文导入规则变化就会把英文计数当成中文提频，
        // 触发不必要的块内重排。
        let user_pinyins: std::collections::HashSet<String> = user_stats
            .zh_pinyins()
            .into_iter()
            .map(str::to_string)
            .collect();
        // FST 模式下用户词 overlay 全量入内存：热路径靠它，不再每键查 SQLite。
        // 纯 SQLite 模式下 `index` 本身就含用户词，overlay 留空。
        let user_overlay = if store.is_some() {
            let mut overlay = Self::load_user_overlay(&conn)?;
            for e in &mut overlay {
                e.eff =
                    e.freq.max(0) as u64 + user_bonus_of(&user_stats, today, &e.pinyin, &e.text);
            }
            overlay
        } else {
            Vec::new()
        };
        let total_freq = Self::query_total_freq(&conn);
        let english = Self::load_english(&conn, &user_stats, today)?;
        // 跨语言分数尺：各语言取最大有效频率做归一化基准。
        //
        // 为什么必须分开取最大值：中文词条频率 10⁵~10⁷、英文词频 10³ 量级，
        // 直接比原始数值等于「中文恒赢」，退化成按语言分块。对数归一化后两边
        // 都是「在本语言内部的相对位置」，才谈得上按分数竞争。
        // 基准 = 库内语料最大频 与 内存条目最大有效频率 取大者。
        // 语料最大值必须查库：FST 模式下 index 是空的，只看内存会把基准算成
        // 「用户词里最大的那条」，中文分数整体贴近上限、英文被系统性压住。
        let zh_max = Self::query_max_freq(&conn, "phrase").max(
            index
                .iter()
                .map(|e| e.eff)
                .chain(user_overlay.iter().map(|e| e.eff))
                .max()
                .unwrap_or(0),
        );
        let en_max = Self::query_max_freq(&conn, "english")
            .max(english.iter().map(|e| e.eff).max().unwrap_or(0));
        Ok(Self {
            zh_max,
            en_max,
            conn,
            index,
            abbrev_index,
            store,
            user_overlay,
            user_pinyins,
            total_freq,
            english,
            user_stats,
            today,
            vocab_ids: HashMap::new(),
            lm_ctx: None,
            lm_counts: HashMap::new(),
            lm_generation: 0,
            lm_warned: false,
            db_path: path.to_path_buf(),
            writer: LearnWriter::default(),
            context_recall: ContextRecall::new(),
            current_input_reading: RefCell::new(None),
            fragment_cache: RefCell::new(Vec::new()),
            realtime_bigram: HashMap::new(),
            lm_prev_text: None,
            context_tail: None,
        })
    }

    /// 英文表全量入内存。排序交给 SQL：`lower(text)` 就是匹配键（text 恒为 ASCII，
    /// SQLite 的 `lower()` 只处理 ASCII，正好是我们要的语义），省掉 Rust 端一次重排。
    /// 表空/不存在时返回空 Vec，开销是一次 prepare。
    ///
    /// `eff` 在这里烘焙（裸频 + 用户提频加成）：加成只影响排序比较器，
    /// 不改词库频率；与中文 `IndexEntry.eff` 的做法一致。
    fn load_english(
        conn: &Connection,
        user_stats: &crate::learn::UserStats,
        today: u64,
    ) -> Result<Vec<EnglishEntry>> {
        let mut stmt = conn
            .prepare("SELECT lower(text), text, freq FROM english ORDER BY 1 ASC, 3 DESC, 2 ASC")?;
        let rows = stmt.query_map(params![], |row| {
            Ok(EnglishEntry {
                key: row.get(0)?,
                text: row.get(1)?,
                freq: row.get(2)?,
                eff: 0,
            })
        })?;
        let mut entries: Vec<EnglishEntry> = rows.collect::<Result<Vec<_>, _>>()?;
        for e in &mut entries {
            e.eff = e.freq.max(0) as u64 + user_bonus_of(user_stats, today, &e.text, &e.text);
        }
        Ok(entries)
    }

    /// 英文词查询：拿**原始按键串**匹配，不做拼音解码（对齐 rime 的 english translator）。
    ///
    /// 两层契约与中文 `lookup_prefix` 一致：层一 = key 恰等于输入的条目，层二 = 以输入为
    /// 前缀的补全；**每层各自**按 `(eff DESC, text ASC)`，层一整体在前，绝不跨层打平——
    /// 否则打 `help` 会被 `helper` 顶掉首候选。层二必须重排：内存 Vec 的主序是 key 升序，
    /// 一段前缀区间里跨了多个 key，天然不是频率序（中文那条路径同理）。
    /// 输入大小写不敏感，上屏文本保持词库原写法（`AA` 与 `aa` 是两条独立条目，都能被 `aa` 命中）。
    ///
    /// `eff` = 裸频 + 用户提频加成（与中文路径同一个 [`user_bonus_of`] 算式）：
    /// 用户常用的英文词往上走，用得少的往下沉。层间仍不跨层打平（层二词再热也不插到
    /// 层一前面），提频只改层内次序。
    pub fn lookup_english(&self, raw: &str, limit: usize) -> Vec<Candidate> {
        if raw.is_empty() || limit == 0 {
            return Vec::new();
        }
        self.refresh_fragment_cache(raw);
        let key = raw.to_ascii_lowercase();
        let start = self
            .english
            .partition_point(|e| e.key.as_str() < key.as_str());
        let exact_len = self.english[start..]
            .iter()
            .take_while(|e| e.key == key)
            .count();
        // 层一：精确块。整块先取全再按分数重排再截断——被提频抬上来的词若在
        // 截断之外就被丢了（同 lookup_prefix 的层一处理）。层归属在这里定型，
        // 不靠返回顺序推断。
        let ranker = self.ranker();
        let mut out: Vec<Candidate> = self.english[start..start + exact_len]
            .iter()
            .map(|e| Self::english_to_candidate(e).with_layer(Layer::Exact))
            .collect();
        ranker.sort_within_batch(&mut out);
        out.truncate(limit);
        let mut comps: Vec<Candidate> = self.english[start + exact_len..]
            .iter()
            .take_while(|e| e.key.starts_with(&key))
            .map(|e| Self::english_to_candidate(e).with_layer(Layer::Completion))
            .collect();
        // 批内重排（最坏 `a` 命中 ~4k 条，release 下几 µs）；哪天真嫌慢再换 top-k 堆。
        ranker.sort_within_batch(&mut comps);
        comps.truncate(limit - out.len());
        out.extend(comps);
        out
    }

    /// 英文词候选：`eff` = 裸频 + 用户提频加成（同一 [`user_bonus_of`] 算式，
    /// 开库/学习时烘焙进条目，热路径只比字段）。
    ///
    /// 加成的键 = `(词, 词)`：与 `english_to_candidate` 的读音键/`text` 同为词本身，
    /// 于是学习侧写入的键与这里查询的键天然对齐（见 [`Dict::learn_english`]）。
    ///
    /// 层与语言在这里定型（`Lang::En` + 由调用方覆盖的层）：英文候选的层判断不再
    /// 依赖「读音键 == 按键串」这条隐含约定，跨语言顺序由 `Lang` 在排序键高位表达。
    fn english_to_candidate(e: &EnglishEntry) -> Candidate {
        Candidate::en(e.text.clone(), e.freq.max(0) as u64, e.eff)
    }

    /// 英文词学习：把用户实际用过的英文词记进 `user_stats`，供 [`Self::lookup_english`]
    /// 的提频加成读取。
    ///
    /// - `word` 必须是**词库 `english` 表里存在的词**（大小写不敏感，按 `lower(text)`
    ///   匹配）：不在表里的串（如 `zxqv`）直接忽略，不写任何记录——否则任意误打的
    ///   串都会进用户词表污染候选。
    /// - 计数键 = `(词, 词)`，与 `english_to_candidate` 的 `pinyin`/`text` 一致，
    ///   故加成在查询侧能被查到。
    /// - 词的大小写按**词库写法**记（`AA` 与 `aa` 是两条独立条目，各记各的）：
    ///   同一小写键下有多个写法时取词频最高的那条，键与查询侧命中的是同一条。
    /// - 持久化写 `kime_kv`（key = `词\t词`，value = `n,day`），与中文学习同一张旁表，
    ///   但**不碰 `phrase` 表**：英文词不是拼音，进 `phrase` 会污染中文前缀查询。
    ///
    /// 返回是否真的记了（词库校验失败返回 false）。
    pub fn learn_english(&mut self, word: &str) -> bool {
        let Some(spelling) = self.english_spelling(word) else {
            return false;
        };
        self.today = today_days();
        // 英文键由类型表达：`LearnKey::En`，永不与中文键混淆，也永不进 phrase 表。
        let key = crate::learn::LearnKey::En {
            word: spelling.clone(),
        };
        let n = self.user_stats.record(key, self.today);
        self.enqueue_english_learn(&spelling, n, self.today);
        self.rebake_english_eff(&spelling);
        true
    }

    /// 英文词 → 词库写法（大小写不敏感匹配）。
    ///
    /// **优先完全同形的写法**（用户敲 `aa` 记在 `aa` 上、`AA` 记在 `AA` 上——
    /// `AA` 与 `aa` 是两条独立条目，各记各的）；没有同形的才退到大小写不敏感的
    /// 首条（词频最高，`load_english` 的排序即 freq DESC）。
    /// 不在表里的词返回 `None`——这正是防污染闸门（乱串不入表）。
    fn english_spelling(&self, word: &str) -> Option<String> {
        if word.is_empty() {
            return None;
        }
        let key = word.to_ascii_lowercase();
        let start = self
            .english
            .partition_point(|e| e.key.as_str() < key.as_str());
        let group: Vec<&EnglishEntry> = self.english[start..]
            .iter()
            .take_while(|e| e.key == key)
            .collect();
        // 完全同形优先；没有同形的才退到首条（词频最高，`load_english` 的排序即 freq DESC）。
        group
            .iter()
            .find(|e| e.text == word)
            .or_else(|| group.first())
            .map(|e| e.text.clone())
    }

    /// 英文词的使用次数（诊断/测试用）：该词在 `user_stats` 里的累计次数。
    ///
    /// 键与 [`Self::learn_english`] 写入的一致（`(词, 词)`，词为词库写法）。
    /// 没学过返回 `None`。
    pub fn english_usage(&self, word: &str) -> Option<u64> {
        let spelling = self.english_spelling(word)?;
        self.user_stats
            .count_of(&crate::learn::LearnKey::En { word: spelling })
            .map(|(n, _)| n)
    }

    /// 重烘该词的 `eff`（学习后当帧生效，与中文 `learn_mem` 同语义）。
    fn rebake_english_eff(&mut self, spelling: &str) {
        let today = self.today;
        // 先算加成再改条目：`self.english` 的可变借用与 `self.user_stats` 的只读
        // 借用不能同时存在（同一 `self`）。
        let bonus = user_bonus_of(&self.user_stats, today, spelling, spelling);
        let mut max_eff = 0;
        for e in self.english.iter_mut().filter(|e| e.text == spelling) {
            e.eff = e.freq.max(0) as u64 + bonus;
            max_eff = max_eff.max(e.eff);
        }
        self.bump_en_max(max_eff);
    }

    /// 英文学习持久化入队（键路径零 SQLite 写）。
    ///
    /// 只写 `kime_kv` 计数（英文词不进 `phrase`），值是**绝对值** `n,day`——
    /// 队列丢一个 job 后下一个 job 仍收敛到最新计数。
    fn enqueue_english_learn(&mut self, word: &str, n: u64, day: u64) {
        let job = LearnJob::EnglishLearn {
            word: word.to_string(),
            n,
            day,
        };
        self.writer.submit(&self.db_path, job);
    }

    /// 读出全部 `user = 1` 行，按 pinyin ASC 排序（前缀二分要求）。
    fn load_user_overlay(conn: &Connection) -> Result<Vec<IndexEntry>> {
        let mut stmt = conn.prepare(
            "SELECT pinyin, text, freq, abbrev, user FROM phrase WHERE user = 1 ORDER BY pinyin ASC",
        )?;
        let rows = stmt.query_map(params![], |row| {
            Ok(IndexEntry {
                pinyin: row.get(0)?,
                text: row.get(1)?,
                freq: row.get(2)?,
                abbrev: row.get(3)?,
                eff: 0,
                user: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// 读出用户提频计数（`kime_kv` 全表，几十行）。value 格式 n,day；
    /// 解析不出的行跳过（该表只有 learn 写，出现坏行说明文件被外部改过）。
    ///
    /// 磁盘格式不变（`pinyin \t text`）：语言标签由 [`crate::learn::LearnKey::from_storage`]
    /// 在载入时重建，因此老数据可直接读回，无需迁移。
    fn load_user_stats(conn: &Connection) -> Result<crate::learn::UserStats> {
        let mut stmt = conn.prepare("SELECT key, value FROM kime_kv")?;
        let rows = stmt.query_map(params![], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for (key, value) in rows.collect::<Result<Vec<_>, _>>()? {
            let Some((pinyin, text)) = key.split_once('\t') else {
                continue;
            };
            let mut parts = value.split(',');
            let (Some(n), Some(day)) = (
                parts.next().and_then(|s| s.parse().ok()),
                parts.next().and_then(|s| s.parse().ok()),
            ) else {
                continue;
            };
            out.push(((pinyin.to_string(), text.to_string()), (n, day)));
        }
        Ok(crate::learn::UserStats::from_storage(out))
    }

    /// 候选排序比较器：委托给排序内核（唯一实现）。
    ///
    /// 保留这个方法是因为 `lm_boost` 是词典的上下文加成来源，由它实现
    /// [`crate::rank::CtxBoost`]；比较本身只在 [`crate::rank::Ranker`] 里发生，
    /// 这里不再有第二份比较逻辑。
    pub fn ranker(&self) -> crate::rank::Ranker<'_> {
        crate::rank::Ranker::new(self, crate::rank::ScoreScale::new(self.zh_max, self.en_max))
    }

    /// 排序分数尺（跨语言可比）。见 [`crate::rank::ScoreScale`]。
    pub fn score_scale(&self) -> crate::rank::ScoreScale {
        crate::rank::ScoreScale::new(self.zh_max, self.en_max)
    }

    /// 抬高中文基准（导入/学习之后调用）。
    fn bump_zh_max(&mut self, eff: u64) {
        if eff > self.zh_max {
            self.zh_max = eff;
        }
    }

    /// 抬高英文基准（导入/学习之后调用）。
    fn bump_en_max(&mut self, eff: u64) {
        if eff > self.en_max {
            self.en_max = eff;
        }
    }

    /// 候选的排序用有效频率（裸频 + 用户学习加成，开库/learn 时烘焙）。
    /// 只读访问器：加成永远不写回 `freq`（导出契约）。
    pub fn effective_freq(&self, c: &Candidate) -> u64 {
        c.eff
    }

    /// (分数 + LM 上下文加成 DESC, text ASC) —— 候选层的统一排序键。
    /// 比较实现见 [`crate::rank::Ranker::compare`]（唯一比较器）。
    #[allow(dead_code)]
    fn cand_cmp(&self, a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
        self.ranker().compare(a, b)
    }

    /// 语料总词频。见字段注释：为什么在开库时算。
    pub fn total_freq(&self) -> u64 {
        self.total_freq
    }

    /// 是否挂载了 FST 复合层（诊断/测试用）。
    pub fn has_fst_store(&self) -> bool {
        self.store.is_some()
    }

    /// 内存排序索引只读视图（诊断/测试用）。
    pub fn index(&self) -> &[IndexEntry] {
        &self.index
    }

    /// 某表的最大频率（跨语言分数归一化的基准之一）。
    ///
    /// 为什么必须查库而不是只看内存索引：FST 模式下内存里只有用户词 overlay
    /// （个位到几十条），拿它当基准会让中文分数整体贴近上限、英文被系统性压住——
    /// 而 FST 是生产路径（dict.bin 与词库同库存在）。
    /// 无 freq 索引，2M 行约 0.4s；只在开库/导入时各一次，不在键路径。
    fn query_max_freq(conn: &Connection, table: &str) -> u64 {
        let sql = format!("SELECT COALESCE(MAX(freq), 0) FROM {table}");
        conn.query_row(&sql, [], |r| r.get::<_, i64>(0))
            .map(|m| m.max(0) as u64)
            .unwrap_or(0)
    }

    fn query_total_freq(conn: &Connection) -> u64 {
        // rusqlite 不为 u64 实现 FromSql，SUM 只能按 i64 取
        conn.query_row("SELECT COALESCE(SUM(freq), 0) FROM phrase", [], |r| {
            r.get::<_, i64>(0)
        })
        .map(|s| s.max(1) as u64)
        .unwrap_or(1)
    }

    /// 导入 rime-ice `.dict.yaml`：解析 TSV 正文（文字\t拼音\t频率）。
    ///
    /// 正文里两类行直接丢弃（见循环内注释）：`text` 以 `#` 开头的注释行、
    /// `pinyin` 含非 `a-z`/空格 的数字读法行。
    ///
    /// 返回值 = 实际写入的行数（新插入 + 频率被抬高的）。冲突但频率没涨的行不计入，
    /// 因此重复导入同一文件返回 0，幂等语义与旧实现一致。
    pub fn import(&mut self, dict_yaml: impl AsRef<Path>) -> Result<usize> {
        let lines = dict_body_lines(dict_yaml.as_ref())?;
        let tx = self.conn.transaction()?;
        let mut count = 0usize;
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let mut cols = line.split('\t');
            // 下面两道是源数据卫生过滤（rime-ice 正文里混着两类不能上屏的行）：
            // ① text 以 `#` 开头的是被注释掉的示例行（库里 11,726 条，`# 那`/nei 频率
            //    高达 9,929,703），频率修好前被 0 压着看不见，修好后会直接顶在首候选；
            // ② 引擎的组合缓冲只收 a-z（数字键是页内选词），所以 `pinyin = "100"` 的
            //    数字读法条目（tencent.dict.yaml 全表 980,961 条 = 旧库 51%）永远查不到，
            //    只会让 dict.bin 胖一倍。
            let text = match cols.next() {
                Some(t) if !t.is_empty() && !t.starts_with('#') => t,
                _ => continue,
            };
            let pinyin = match cols.next() {
                Some(p) if !p.is_empty() => p,
                _ => continue,
            };
            let freq: i64 = match cols.next() {
                Some(s) => s.parse().unwrap_or(0),
                None => 0,
            };
            if !pinyin.bytes().all(|b| b.is_ascii_alphabetic() || b == b' ') {
                continue;
            }
            let syllables: Vec<&str> = pinyin.split_whitespace().collect();
            let joined = syllables.join("'");
            let abbrev: String = syllables.iter().filter_map(|s| s.chars().next()).collect();
            let abbrev = abbrev.to_lowercase();
            let added = tx.execute(
                "INSERT INTO phrase(pinyin, text, freq, abbrev, user) VALUES (?1, ?2, ?3, ?4, 0)
                 ON CONFLICT(pinyin, text) DO UPDATE SET freq = excluded.freq
                 WHERE user = 0 AND freq < excluded.freq",
                params![joined, text, freq, abbrev],
            )?;
            count += added;
        }
        tx.commit()?;
        // Rebuild memory index after import
        let mut stmt = self.conn.prepare(
            "SELECT pinyin, text, freq, abbrev, user FROM phrase ORDER BY pinyin ASC, freq DESC, text ASC",
        )?;
        let rows = stmt.query_map(params![], |row| {
            Ok(IndexEntry {
                pinyin: row.get(0)?,
                text: row.get(1)?,
                freq: row.get(2)?,
                abbrev: row.get(3)?,
                eff: 0,
                user: row.get(4)?,
            })
        })?;
        let mut index: Vec<_> = rows.collect::<Result<Vec<_>, _>>()?;
        // stmt 借用 self.conn，必须在改 self 之前释放，否则后面的 self.index 赋值
        // 与 self.bump_zh_max 会和它冲突。
        drop(stmt);
        for e in &mut index {
            e.eff = if e.user == 1 {
                e.freq.max(0) as u64
                    + user_bonus_of(&self.user_stats, self.today, &e.pinyin, &e.text)
            } else {
                e.freq.max(0) as u64
            };
        }
        self.index = index;
        let mut abbrev_index = self.index.clone();
        abbrev_index.sort_by(|a, b| {
            a.abbrev
                .cmp(&b.abbrev)
                .then_with(|| b.eff.cmp(&a.eff))
                .then_with(|| a.text.cmp(&b.text))
        });
        self.abbrev_index = abbrev_index;
        // 批量灌词后总量变了，缓存必须跟着刷新（否则刚导入的库整句代价用旧概率）
        self.total_freq = Self::query_total_freq(&self.conn);
        // 跨语言分数基准跟着抬高：导入的词可能比原有最大频还高，
        // 不更新的话新词的校准分会算错（导入在 open 之后发生，开库时算的基准已过期）。
        // FST 模式下 index 是空的，所以同时查一次库内最大值。
        self.bump_zh_max(Self::query_max_freq(&self.conn, "phrase"));
        if let Some(max) = self.index.iter().map(|e| e.eff).max() {
            self.bump_zh_max(max);
        }
        Ok(count)
    }

    /// 导入 rime-ice 英文表（`en_dicts/en.dict.yaml`、`en_ext.dict.yaml`；格式同样是
    /// `文字\t拼音\t权重`，但英文表基本没有第三列，freq 落 0 是正常状态）。
    ///
    /// **英文绝不进 `phrase`**：那张表的 `UNIQUE(pinyin, text)` 与 `idx_phrase_pinyin`
    /// 服务的是拼音前缀查询，混进 `hello`/`help` 这类纯字母 key 之后，打 `he` 就会在
    /// 中文候选里捞出英文词（`help` 频次高于「何」）—— 正是本设计最容易坏的地方。
    /// 所以走独立表 + 独立内存索引，匹配规则也不同（原始按键串直查，不做音节切分）。
    ///
    /// 清洗规则（实测两表 24,999 行正文 → 21,967 行通过 → 去重后 **21,708 条**入库）：
    /// - 只收 `text` 为**纯 ASCII 字母**的条目，一条谓词同时挡掉两类垃圾：
    ///   `# ab\tab` 形式的注释行（rime-ice 用它注释掉不想要的词，1,877 条）和带
    ///   `'` `-` `.` 空格 数字 非 ASCII 的条目（`he'll`、`e-mail`、`.NET`、`iPhone 17`、
    ///   `café`，1,155 条）。后者本来就打不出来——引擎的组合缓冲只收 a-z——收了只会给
    ///   二分的排序键添符号分支。`en_ext` 里那几十条靠 `拼音` 列做同形别名的行
    ///   （`he'll`→`hell`、`.NET`→`net`）连带失效；要支持得改成按拼音列匹配，是另一件事。
    /// - 空 `text` 必须单独挡：`starts_with("")` 会把全表变成前缀命中。
    /// - 大小写：`text` 原样入库用于上屏，匹配键在读取时小写化。主键是 `text`，所以
    ///   `AA` 与 `aa` 是两行，互不覆盖。
    ///
    /// 同 `text` 重复时频率取大（与 `import` 同策略），因此重复导入返回 0，幂等。
    /// 返回实际写入行数，并刷新内存索引（同进程导入后立刻可查）。
    pub fn import_english(&mut self, dict_yaml: impl AsRef<Path>) -> Result<usize> {
        let lines = dict_body_lines(dict_yaml.as_ref())?;
        let tx = self.conn.transaction()?;
        let mut count = 0usize;
        for line in lines {
            let mut cols = line.split('\t');
            let text = match cols.next() {
                Some(t) => t,
                None => continue,
            };
            if text.is_empty() || !text.bytes().all(|b| b.is_ascii_alphabetic()) {
                continue;
            }
            cols.next(); // 第二列是 rime 的 `拼音`（英文表里它就是词本身，偶尔是 `hell` 这种
                         // 同形别名）—— 我们按 text 匹配，不参与，直接跳过；第三列才是权重。
            let freq: i64 = cols.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
            count += tx.execute(
                "INSERT INTO english(text, freq) VALUES (?1, ?2)
                 ON CONFLICT(text) DO UPDATE SET freq = excluded.freq WHERE english.freq < excluded.freq",
                params![text, freq],
            )?;
        }
        tx.commit()?;
        self.english = Self::load_english(&self.conn, &self.user_stats, self.today)?;
        self.bump_en_max(Self::query_max_freq(&self.conn, "english"));
        if let Some(max) = self.english.iter().map(|e| e.eff).max() {
            self.bump_en_max(max);
        }
        Ok(count)
    }

    /// 用户词 overlay：精确 pinyin 命中。`user_overlay` 按 pinyin ASC，二分定位后线性取等值段。
    ///
    /// 层归属 = 精确层：用户词只在「读音与输入完全一致」时进精确层，学习提频不得让
    /// 它借补全层插队（`user_word_cannot_jump_layers`）。
    fn overlay_exact(&self, joined: &str) -> Vec<Candidate> {
        let start = self
            .user_overlay
            .partition_point(|e| e.pinyin.as_str() < joined);
        self.user_overlay[start..]
            .iter()
            .take_while(|e| e.pinyin == joined)
            .map(|e| Self::entry_to_candidate(e).with_layer(Layer::Exact))
            .collect()
    }

    /// 用户词 overlay：pinyin 前缀区间 `[lower, upper)`。`upper` 为 None 表示无上界（前缀以 z 结尾）。
    /// 层由调用方按精确/补全分别覆盖（同一段前缀可能同时喂给两层）。
    fn overlay_prefix(&self, lower: &str, upper: Option<&str>) -> Vec<Candidate> {
        let start = self
            .user_overlay
            .partition_point(|e| e.pinyin.as_str() < lower);
        self.user_overlay[start..]
            .iter()
            .take_while(|e| match upper {
                Some(up) => e.pinyin.as_str() < up,
                None => e.pinyin.starts_with(lower),
            })
            .map(Self::entry_to_candidate)
            .collect()
    }

    /// 用户词 overlay 的层二（补全）切分，与 store 的两层谓词完全一致：
    /// tail 为空 → 只收以 `joined + "'"` 开头的 key；tail 非空 → 开区间去掉精确 key。
    fn overlay_comps(&self, joined: &str, tail: &str) -> Vec<Candidate> {
        let mut v = if tail.is_empty() {
            let lower = format!("{joined}'");
            let upper = crate::model::prefix_upper_bound(&lower);
            self.overlay_prefix(&lower, upper.as_deref())
        } else {
            let upper = crate::model::prefix_upper_bound(joined);
            let mut v = self.overlay_prefix(joined, upper.as_deref());
            v.retain(|c| c.pinyin != joined);
            v
        };
        for c in &mut v {
            c.layer = Layer::Completion;
        }
        v
    }

    /// 用户词 overlay：abbrev 前缀命中。overlay 按 pinyin 排序，abbrev 只能全扫——
    /// 集合是个位到几十条，扫一遍比维护第二份排序便宜。层 = 兜底（缩写只在主查询
    /// 无结果时使用）。
    fn overlay_abbrev(&self, initials: &str) -> Vec<Candidate> {
        self.user_overlay
            .iter()
            .filter(|e| e.abbrev.starts_with(initials))
            .map(|e| Self::entry_to_candidate(e).with_layer(Layer::Fallback))
            .collect()
    }

    /// 中文词典条目 → 候选。层由调用方按产生阶段覆盖（精确块 / 补全块 / 缩写兜底）。
    fn entry_to_candidate(e: &IndexEntry) -> Candidate {
        let source = if e.user == 1 {
            SourceKind::User
        } else {
            SourceKind::Dict
        };
        Candidate::zh(
            e.text.clone(),
            e.pinyin.clone(),
            e.freq.max(0) as u64,
            e.eff,
        )
        .with_source(source)
    }

    /// 单层合并：用户词覆盖基底同文本（以用户词频为准），结果仅在本层内按
    /// (freq DESC, text ASC) 截断。层间顺序由调用方（`lookup_prefix`）拼接，
    /// 这里绝不允许拿到跨层的列表来排序——那会把「精确命中先于补全」打平。
    fn merge_overlay(
        &self,
        base: Vec<Candidate>,
        user: Vec<Candidate>,
        limit: usize,
    ) -> Vec<Candidate> {
        if user.is_empty() {
            // 绝大多数按键走这里：基底已是 freq DESC 有序，不必重排。
            let mut base = base;
            base.truncate(limit);
            return base;
        }
        // 基底先入（先到先得），用户词后入（同文本以用户词为准）。
        let mut merged: HashMap<String, Candidate> =
            HashMap::with_capacity(base.len() + user.len());
        base.into_iter().for_each(|c| {
            merged.entry(c.text.clone()).or_insert(c);
        });
        user.into_iter().for_each(|c| {
            merged.insert(c.text.clone(), c);
        });
        let mut candidates: Vec<Candidate> = merged.into_values().collect();
        candidates.sort_by(|a, b| self.cand_cmp(a, b));
        candidates.truncate(limit);
        candidates
    }

    pub fn lookup(&self, reading: &[String], limit: usize) -> Result<Vec<Candidate>> {
        if reading.is_empty() {
            return Ok(Vec::new());
        }
        let joined = reading.join("'");
        self.refresh_fragment_cache(&joined);
        if let Some(store) = &self.store {
            let base = store.lookup_exact(reading, limit);
            let user_candidates = self.overlay_exact(&joined);
            return Ok(self.merge_overlay(base, user_candidates, limit));
        }
        let start = self.index.partition_point(|e| e.pinyin < joined);
        let mut candidates = Vec::new();
        for entry in &self.index[start..] {
            if entry.pinyin != joined {
                break;
            }
            candidates.push(Self::entry_to_candidate(entry).with_layer(Layer::Exact));
        }
        self.ranker().sort(&mut candidates);
        candidates.truncate(limit);
        Ok(candidates)
    }

    /// 由上屏文本反查读音（上下文感知：光标前已上屏的末词 → 读音 + 频率）。
    ///
    /// 精确 `text` 匹配 phrase 表，返回 `(pinyin, freq)`，按 (freq DESC, pinyin ASC) 排序
    /// LIMIT `limit`——调用方据此选出「上文末词」作为组句种子（见 `lattice::Seed`）。
    /// 一条 SQL 走 `idx_phrase_text` 点查（O(log n)），每键最多 4 次（末 1..=4 字窗口）。
    ///
    /// 两条存储路径共用同一条 SQL：dict.bin 只存 pinyin 键（builder 不建 text 反查索引），
    /// 所以 FST 模式下文本反查也只能落回 SQLite——该模式 `self.index` 为空，更没有第二条路。
    /// 返回库内裸频：上文先验是语料级统计，不掺用户提频加成（`learn` 提的频不改变语料先验）。
    pub fn readings_of_text(&self, text: &str, limit: usize) -> Result<Vec<(String, u64)>> {
        if text.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT pinyin, freq FROM phrase WHERE text = ?1 ORDER BY freq DESC, pinyin ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![text, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?.max(0) as u64,
            ))
        })?;
        rows.collect()
    }

    /// 前缀查询（两层合并）：层一 = key 恰等于输入（joined）的词条，层二 = 合法补全。
    /// 每层内部 `(freq DESC, text ASC)`，拼接时层一整体在前——合并阶段绝不允许
    /// 对跨层列表做一次性频率排序（那正是「我们去 被 我们确信 压住」的形态）。
    /// limit 分配：层一不满时层二填满到 limit；
    /// 层一溢出时——仅当 tail 非空（还在打最后一个音节）——层二至少保留一半名额，
    /// 「`mi` 仍要够得着 `min`/`ming` 的字」是显式验收，否则 `mi` 这种几百字的精确块
    /// 会把补全整段挤出列表。tail 为空 = 音节已收口，层一独占到底。
    ///
    /// FST 路径与纯 SQLite 内存索引路径必须同语义（上一轮 `lookup_exact` 的 bug 就出在
    /// 两条路径不一致）。用户词 overlay 按层合并：只有 pinyin == joined 的词进层一，
    /// 不许在层二借高频插队到精确命中之前；用户词与层一基底同文本时频率以用户词为准、
    /// 层归属沿用原条目（不许跨层跳跃），且任何与层一重复的文本不进层二。
    pub fn lookup_prefix(
        &self,
        syllables: &[String],
        tail: &str,
        limit: usize,
    ) -> Result<Vec<Candidate>> {
        let mut joined = syllables.join("'");
        if !tail.is_empty() {
            if !joined.is_empty() {
                joined.push('\'');
            }
            joined.push_str(tail);
        }
        if joined.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        self.refresh_fragment_cache(&joined);
        if let Some(store) = &self.store {
            let user_exact = self.overlay_exact(&joined);
            let mut user_comps = self.overlay_comps(&joined, tail);
            // 用户词 overlay 只在 FST 路径需要：纯 SQLite 路径的 `index` 本身
            // 就含用户词（且已烘好 eff），再查 overlay 是纯浪费（审查发现：
            // overlay_comps 还会为前缀上界分配一个 Vec<char>）。
            let (base_exact, base_comps) = store.lookup_prefix_layers(syllables, tail, limit);
            // 存储层只负责取词，层归属在这里定型：两条存储路径因此不可能给出
            // 不同的层语义（历史 bug 正出在「谁定层」分散在两处）。
            let mut base_exact: Vec<Candidate> = base_exact
                .into_iter()
                .map(|c| c.with_layer(Layer::Exact))
                .collect();
            let mut base_comps: Vec<Candidate> = base_comps
                .into_iter()
                .map(|c| c.with_layer(Layer::Completion))
                .collect();
            // 用户词文本命中层一基底（哪怕学的时候用的是别的读音）：
            // 频率以用户词为准，层归属沿用原条目，并从层二候选里剔除，不许复制。
            let mut exact_dirty = false;
            user_comps.retain(|u| match base_exact.iter_mut().find(|b| b.text == u.text) {
                Some(b) => {
                    b.freq = u.freq;
                    exact_dirty = true;
                    false
                }
                None => true,
            });
            if exact_dirty {
                self.ranker().sort(&mut base_exact);
            }
            // 层一预算 = 总名额 − 补全保留名额；tail 为空（音节已收口）时层一独占。
            let reserve = crate::rank::reserve_for_completion(limit);
            let l1_cap = if tail.is_empty() {
                limit
            } else {
                crate::rank::exact_budget(limit, reserve)
            };
            let mut out = self.merge_overlay(base_exact, user_exact, l1_cap);
            let l1_texts: std::collections::HashSet<&str> =
                out.iter().map(|c| c.text.as_str()).collect();
            base_comps.retain(|c| !l1_texts.contains(c.text.as_str()));
            user_comps.retain(|c| !l1_texts.contains(c.text.as_str()));
            let rest = limit - out.len().min(limit);
            out.extend(self.merge_overlay(base_comps, user_comps, rest));
            out.truncate(limit);
            return Ok(out);
        }

        // 纯 SQLite 路径：`index` 按 (pinyin ASC, freq DESC, text ASC) 排好，
        // 层一 = joined 的精确块（切片内已 freq 降序），层二 = 区间其余部分重排。
        let start = self.index.partition_point(|e| e.pinyin < joined);
        let exact_len = self.index[start..]
            .iter()
            .take_while(|e| e.pinyin == joined)
            .count();
        // 层一：精确块。块内 eff 重排只在「打的是学到过的拼音」时发生（user_pinyins
        // O(1) 判断）——开库不再整索引重排后，boost 在这里按需生效；此时必须先取
        // 整块再截断，否则被 boost 抬进 top 的词还在 take 之外就被丢了。
        let cap = if tail.is_empty() {
            limit
        } else {
            crate::rank::exact_budget(limit, crate::rank::reserve_for_completion(limit))
        };
        let ranker = self.ranker();
        let mut out: Vec<Candidate> = if self.user_pinyins.contains(joined.as_str()) {
            let mut block: Vec<Candidate> = self.index[start..start + exact_len]
                .iter()
                .map(|e| Self::entry_to_candidate(e).with_layer(Layer::Exact))
                .collect();
            ranker.sort(&mut block);
            block.truncate(cap);
            block
        } else {
            self.index[start..]
                .iter()
                .take_while(|e| e.pinyin == joined)
                .map(|e| Self::entry_to_candidate(e).with_layer(Layer::Exact))
                .take(cap)
                .collect()
        };
        let (lo, hi) = if tail.is_empty() {
            let lower = format!("{joined}'");
            let upper = crate::model::prefix_upper_bound(&lower).unwrap_or_else(|| lower.clone());
            (
                self.index.partition_point(|e| e.pinyin < lower),
                self.index.partition_point(|e| e.pinyin < upper),
            )
        } else {
            let upper = crate::model::prefix_upper_bound(&joined).unwrap_or_else(|| joined.clone());
            (
                start + exact_len,
                self.index.partition_point(|e| e.pinyin < upper),
            )
        };
        // 与 FST 路径同语义：用户词（另一读音下学的）与层一同文本时频率以用户词为准、
        // 层归属不变；层二不得重复层一已展示的文本。
        let mut l1_dirty = false;
        for entry in &self.index[lo..hi] {
            if entry.user != 1 {
                continue;
            }
            if let Some(o) = out.iter_mut().find(|o| o.text == entry.text) {
                if o.freq != entry.freq.max(0) as u64 {
                    o.freq = entry.freq.max(0) as u64;
                    l1_dirty = true;
                }
            }
        }
        if l1_dirty {
            ranker.sort(&mut out);
        }
        let mut comps: Vec<Candidate> = self.index[lo..hi]
            .iter()
            .map(|e| Self::entry_to_candidate(e).with_layer(Layer::Completion))
            .filter(|c| !out.iter().any(|o| o.text == c.text))
            .collect();
        ranker.sort(&mut comps);
        comps.truncate(limit - out.len());
        out.extend(comps);
        Ok(out)
    }

    /// 声母缩写查询
    pub fn lookup_abbrev(&self, initials: &str, limit: usize) -> Result<Vec<Candidate>> {
        if initials.is_empty() {
            return Ok(Vec::new());
        }
        if !initials.bytes().all(|b| b.is_ascii_lowercase()) {
            return Ok(Vec::new());
        }
        if let Some(store) = &self.store {
            // FST 模式：使用 FST 的 abbrev 索引，用户词从内存 overlay 补
            let candidates = store.lookup_abbrev(initials, limit);
            let user_candidates = self.overlay_abbrev(initials);
            Ok(self.merge_overlay(candidates, user_candidates, limit))
        } else {
            // 回退到内存 abbrev_index
            let lower = self
                .abbrev_index
                .partition_point(|e| e.abbrev.as_str() < initials);
            let upper_char = (initials.as_bytes()[initials.len() - 1] + 1) as char;
            let upper_prefix = format!("{}{}", &initials[..initials.len() - 1], upper_char);
            let upper = self
                .abbrev_index
                .partition_point(|e| e.abbrev.as_str() < upper_prefix.as_str());
            let slice = &self.abbrev_index[lower..upper];
            let mut candidates: Vec<Candidate> = slice
                .iter()
                .map(|e| Self::entry_to_candidate(e).with_layer(Layer::Fallback))
                .collect();
            // 与 FST 分支同语义：缩写查询也按有效频率（裸频 + 提频加成）排序，
            // 否则用户打缩写时「使用频率高的在前」不生效，两条路径行为分裂。
            self.ranker().sort(&mut candidates);
            candidates.truncate(limit);
            Ok(candidates)
        }
    }

    /// 用户词前 N：user=1 行按 freq DESC, text ASC（engine 个性化排序用）。
    pub fn top_user(&self, limit: usize) -> Result<Vec<Candidate>> {
        // FST 模式下 `index` 是空的（词库在 dict.bin 里），用户词只在 overlay 中。
        let source = if self.store.is_some() {
            &self.user_overlay
        } else {
            &self.index
        };
        let mut candidates: Vec<Candidate> = source
            .iter()
            .filter(|e| e.user == 1)
            .map(Self::entry_to_candidate)
            .collect();
        candidates.sort_by(|a, b| b.freq.cmp(&a.freq).then_with(|| a.text.cmp(&b.text)));
        candidates.truncate(limit);
        Ok(candidates)
    }

    /// 学习：内存即时提升 + 同步持久化。dict 级调用方（测试、工具）走这里，
    /// 行为与拆分前一致；选词热路径走 [`Self::learn_mem`] + [`Self::enqueue_learn`]
    /// （#101：键路径零 SQLite 写，见 `learn_writer` 模块）。
    pub fn learn(&mut self, reading: &[String], text: &str) -> Result<()> {
        self.learn_mem(reading, text)?;
        let Some((pinyin, abbrev, n, day)) = self.learn_persist_payload(reading, text) else {
            return Ok(());
        };
        persist_learn(&self.conn, &pinyin, &abbrev, text, n, day)
    }

    /// learn 的**纯内存**部分（#101 拆分）：`user_stats` 计数、FST `user_overlay`、
    /// SQLite `index`/`abbrev_index` 提频与块重排——全部选词当帧生效（2026-09-15
    /// 拍板「使用即提升当场生效」）。库写由 [`persist_learn`] 承担：同步路径见
    /// [`Self::learn`]，键路径见 [`Self::enqueue_learn`]。
    ///
    /// 拆分后本函数先跑、持久化后落，FST 过渡分支的 freq 回读读到的是**写前**值，
    /// 用 +1 镜像 UPSERT（行不存在 = 新词，INSERT 将落 1，等价 `unwrap_or(0)+1`），
    /// 读与写不再互为先后。
    ///
    /// 错误：只有 FST 过渡分支的 freq 回读可能失败（DB I/O/锁）；其余为纯内存操作。
    pub fn learn_mem(&mut self, reading: &[String], text: &str) -> Result<()> {
        if reading.is_empty() {
            return Ok(());
        }
        let joined = reading.join("'");
        let abbrev = abbrev_of(reading);

        // 用户提频计数（方案 B：使用即提升 + 按天时间衰减，rime user_freq 语义）。
        // phrase.freq 保持旧语义（词库频率 + 累计 bump 次数），使用次数 n 与最近使用日
        // 写旁表 kime_kv；加成只进比较器，不改任何导出频率。首用即满额
        // （n×300k，用户拍板 2026-09-15）——选过的词立刻要有存在感；
        // `tests/dict.rs` 钉「等量加成相抵、排序仍由裸频定」。
        self.today = today_days();
        // 中文键由类型表达：`LearnKey::Zh`（读音 + 文本）。与英文键是同一个枚举的
        // 两个变体，因此「英文计数不得触发中文重排」由类型保证，而非内容判据。
        let stats_key = crate::learn::LearnKey::Zh {
            pinyin: joined.clone(),
            text: text.to_string(),
        };
        let _n = self.user_stats.record(stats_key, self.today);

        // FST 模式：dict.bin 只读，用户词的查询数据源是 user_overlay，必须当场同步，
        // 否则刚学的词要等到下次开库才查得到。
        if self.store.is_some() {
            match self
                .user_overlay
                .iter_mut()
                .find(|e| e.pinyin == joined && e.text == text)
            {
                Some(entry) => entry.freq += 1,
                None => {
                    // 这行刚从词库词转成用户词（提权即将落盘），它的真实频率是
                    // 词库频率 + 1，不是 1。持久化还没跑，读回的是写前权威值，
                    // +1 镜像 UPSERT 的 freq+1；行不存在（全新用户词）按 0+1 折算
                    // 成 1，与旧实现「先 INSERT 再回读 1」等价——别让 overlay 把
                    // 候选踢到末尾。
                    let freq: i64 = self
                        .conn
                        .query_row(
                            "SELECT freq FROM phrase WHERE pinyin = ?1 AND text = ?2",
                            params![&joined, text],
                            |row| row.get(0),
                        )
                        .optional()?
                        .unwrap_or(0)
                        + 1;
                    let eff =
                        freq as u64 + user_bonus_of(&self.user_stats, self.today, &joined, text);
                    let pos = self.user_overlay.partition_point(|e| e.pinyin < joined);
                    self.user_overlay.insert(
                        pos,
                        IndexEntry {
                            pinyin: joined,
                            text: text.to_string(),
                            freq,
                            abbrev,
                            eff,
                            user: 1,
                        },
                    );
                }
            }
            // FST 模式下基准同样要跟着抬：用户词 overlay 的 eff 在这里被改，
            // 若只更新内存不更新基准，跨语言校准的分母会偏小，中文分数系统性偏高
            //（审查发现：这条分支以前直接 return，基准停在开库时的值）。
            if let Some(max) = self.user_overlay.iter().map(|e| e.eff).max() {
                self.bump_zh_max(max);
            }
            return Ok(());
        }

        // 纯 SQLite 模式：内存索引就是查询数据源。
        if let Some(entry) = self
            .index
            .iter_mut()
            .find(|e| e.pinyin == joined && e.text == text)
        {
            entry.freq += 1;
            entry.user = 1;
            entry.eff = entry.freq.max(0) as u64
                + user_bonus_of(&self.user_stats, self.today, &entry.pinyin, &entry.text);
            // abbrev_index 同行同步：查询层的 cand_cmp 只读 eff 字段，不同步的话
            // 缩写路径永远看不到本轮提频（旧实现靠 cand_cmp 现算 stats 掩盖）。
            if let Some(ae) = self
                .abbrev_index
                .iter_mut()
                .find(|e| e.pinyin == joined && e.text == text)
            {
                ae.freq = entry.freq;
                ae.user = 1;
                ae.eff = entry.eff;
            }
        } else {
            let eff = 1u64 + user_bonus_of(&self.user_stats, self.today, &joined, text);
            let new_entry = IndexEntry {
                pinyin: joined.clone(),
                text: text.to_string(),
                freq: 1,
                abbrev,
                eff,
                user: 1,
            };
            let pos = self.index.partition_point(|e| e.pinyin < new_entry.pinyin);
            self.index.insert(pos, new_entry.clone());
            let apos = self
                .abbrev_index
                .partition_point(|e| e.abbrev < new_entry.abbrev);
            self.abbrev_index.insert(apos, new_entry);
        }
        // 使用即提升当场生效：层一候选直接按 index 的块序吐，本轮统计变了就得重排
        // joined 块（旧 +1 语义几乎不换序，加成让 n≥2 的选词立刻顶到块首）。
        // joined 在本函数开头已算好，这里不再从键里取读音（键是带语言标签的枚举，
        // 读音只在 `LearnKey::Zh` 变体里）。
        let lo = self
            .index
            .partition_point(|e| e.pinyin.as_str() < joined.as_str());
        let hi = self
            .index
            .partition_point(|e| e.pinyin.as_str() <= joined.as_str());
        self.index[lo..hi].sort_by(entry_cmp);
        // 学习会抬高 eff（可能超过开库时算的基准），基准必须跟着抬，
        // 否则该语言的分母被低估、校准分系统性偏高。
        if let Some(max) = self.index[lo..hi].iter().map(|e| e.eff).max() {
            self.bump_zh_max(max);
        }
        Ok(())
    }

    /// learn 持久化载荷：`(pinyin, abbrev, 使用次数 n, 最近使用日)`。
    /// n/day 取自 [`Self::learn_mem`] 刚写进 `user_stats` 的本轮计数——非空读音
    /// 时该键必在（learn_mem 先更新计数再做任何可能失败的读），返回 None 只可能是
    /// 读音为空。同步 [`Self::learn`] 与 [`Self::enqueue_learn`] 共用。
    fn learn_persist_payload(
        &self,
        reading: &[String],
        text: &str,
    ) -> Option<(String, String, u64, u64)> {
        if reading.is_empty() {
            return None;
        }
        let pinyin = reading.join("'");
        let (n, day) = self.user_stats.count_of(&crate::learn::LearnKey::Zh {
            pinyin: pinyin.clone(),
            text: text.to_string(),
        })?;
        Some((pinyin, abbrev_of(reading), n, day))
    }

    /// commit_log 入队（键路径，#101：零 SQLite 写）。job 只携带原始串，
    /// vocab id 由后台写线程执行时解析；读音/文本为空直接 no-op，与同步
    /// [`Self::log_commit`] 的入口守卫同语义。
    pub(crate) fn enqueue_log_commit(
        &mut self,
        ctx: Option<(&str, &str)>,
        reading: &[String],
        text: &str,
        tail: Option<&str>,
    ) {
        if reading.is_empty() || text.is_empty() {
            return;
        }
        let job = LearnJob::CommitLog {
            ctx: ctx.map(|(t, r)| (t.to_string(), r.to_string())),
            reading: reading.join("'"),
            text: text.to_string(),
            tail: tail.map(str::to_string),
        };
        self.writer.submit(&self.db_path, job);
    }

    /// learn 持久化入队（键路径，#101）。载荷取自 [`Self::learn_persist_payload`]
    /// ——`learn_mem` 刚写进 `user_stats` 的本轮计数；内存提频已当帧生效，
    /// 这里只把 phrase UPSERT + kime_kv 绝对计数交给后台写线程。
    ///
    /// `&mut self` 而非任务书建议的 `&self`：首个 job 需要懒 spawn 线程，
    /// 写入 sender/句柄。
    pub(crate) fn enqueue_learn(&mut self, reading: &[String], text: &str) {
        let Some((pinyin, abbrev, n, day)) = self.learn_persist_payload(reading, text) else {
            return;
        };
        let job = LearnJob::Learn {
            pinyin,
            abbrev,
            text: text.to_string(),
            n,
            day,
        };
        self.writer.submit(&self.db_path, job);
    }

    /// todo/2026-09-18-offline-lm-design.md 第 0 层）。
    ///
    /// `ctx` = 上一次 kime 上屏的词 `(text, reading)`，用来挖掘 bigram。
    /// 用引擎自记的上次提交而非解析 surrounding_text：后者混着非 kime 输入的文本
    /// （粘贴、kime 启动前打的字），bigram 会被污染；前者恒为 kime 自己的提交。
    /// `tail` = 落屏句的上屏前文尾（引擎 context），落 `tail_ctx` 列；
    /// 离线挖掘按词库前缀把它切成前词，补出真实相邻对（#89）。None = 无。
    ///
    /// 失败**不阻塞上屏**：commit_log 缺失只影响离线排序质量，本次输入照常完成。
    pub fn log_commit(
        &mut self,
        ctx: Option<(&str, &str)>,
        reading: &[String],
        text: &str,
        tail: Option<&str>,
    ) {
        // 实时 bigram + 上下文召回（当帧生效，零 SQLite 写）。
        // 持久化（commit_log）由下方 persist_commit_log 独立完成。
        self.commit_context_sync(ctx, reading, text);

        // 失败不阻塞上屏（输入法优先级），但不能静默：日志是离线挖掘唯一
        // 原材料，连失败会掏空排序质量。一次性告警（lm_warned 哨兵防刷屏
        // ——每键都打印会淹掉终端），且绝不 panic（见 warn_once）。
        if let Err(e) = persist_commit_log(
            &self.conn,
            &mut self.vocab_ids,
            ctx,
            &reading.join("'"),
            text,
            tail,
            today_days(),
        ) {
            warn_once(
                &mut self.lm_warned,
                &format!("[kime] commit_log 写入失败（后续不再重复提示）: {e}"),
            );
        }
    }
    /// Commit 同步路径：把本次上屏词写入实时 bigram（realtime_bigram）与上下文召回结构，
    /// 供 lm_boost 当帧生效。持久化（commit_log）由 `log_commit` / `enqueue_log_commit`
    /// 独立完成，此处只改内存，零 SQLite 写，不阻塞上屏。
    ///
    /// 仅中文→中文写入 realtime bigram（英文上下文不污染中文 bigram）；所有 kime 提交
    /// 都记入上下文召回（中文参与 bigram 加成，英文只做历史片段召回）。
    pub(crate) fn commit_context_sync(
        &mut self,
        prev: Option<(&str, &str)>,
        reading: &[String],
        text: &str,
    ) {
        if text.is_empty() {
            return;
        }
        let current_reading = reading.join("'");

        // 实时 bigram：prev_word → current_word，仅中文→中文。
        // realtime_bigram 不被 set_lm_context 清空，与离线 lm_counts 互补。
        let cur_is_cjk = text.chars().next().is_some_and(Self::is_cjk);
        if !current_reading.is_empty() && cur_is_cjk {
            if let Some((prev_text, _)) = prev {
                if prev_text.chars().next().is_some_and(Self::is_cjk) {
                    self.realtime_bigram
                        .entry(prev_text.to_string())
                        .or_default()
                        .entry(current_reading.clone())
                        .and_modify(|c| *c += 1)
                        .or_insert(1);
                }
            }
        }
        // 本次词成为下一次查询的「上一词」。非中文提交不建立中文 bigram 关系，
        // 但要把外键清掉，否则英文词之后仍会按上上个中文词加成。
        self.lm_prev_text = if cur_is_cjk {
            Some(text.to_string())
        } else {
            None
        };

        // 上下文召回：所有 kime 提交都记录（文本 + 读音）。
        self.context_recall.record(text, &current_reading);
    }

    /// 设置当前输入读音（transient），供 lm_boost 的上下文召回匹配用。
    /// 引擎在 lookup 前调用；未设置时召回加成为 0。
    pub(crate) fn set_current_input_reading(&self, reading: Option<&str>) {
        *self.current_input_reading.borrow_mut() = reading.map(str::to_string);
    }

    /// 记录 shell surrounding_text（光标前上文）作为召回线索。
    ///
    /// 与 `context_recall` **分开存**：那套条目的读音是拼音，按读音前缀匹配；
    /// 而 surrounding_text 是汉字文本，与拼音永不匹配。混在一起只会白占 64 条
    /// LRU 名额、淘汰真正能命中的拼音条目（历史 bug）。
    pub(crate) fn record_context_tail(&mut self, text: &str) {
        if text.is_empty() {
            // 空文本 = 上下文断裂（句首 / 光标位置非法）：必须清掉旧段文本，
            // 否则 has_context 恒为真，上一段的 surrounding_text 会继续给新句子
            // 的候选加分（跨段污染，违反「无上下文 = 裸频序」）。
            self.context_tail = None;
            return;
        }
        if self.context_tail.as_deref() == Some(text) {
            return;
        }
        self.context_tail = Some(text.to_string());
    }

    /// 清空 surrounding_text 召回线索（上下文断裂时由引擎调用）。
    pub(crate) fn clear_context_tail(&mut self) {
        self.context_tail = None;
    }

    /// 刷新离线片段缓存：按当前输入读音查询 lm_fragment 表，供 `lm_boost` 的片段召回加成用。
    ///
    /// 两处省查询：
    /// - 读音未变 → 直接返回（同一读音内重复按键不查库）；
    /// - **无上下文** → 清空缓存直接返回。片段加成只在有上下文时才计分
    ///   （见 `lm_boost` 的 `has_context` 门控），此时查库纯属白跑。
    pub(crate) fn refresh_fragment_cache(&self, reading: &str) {
        if !self.has_context() {
            self.set_current_input_reading(Some(reading));
            *self.fragment_cache.borrow_mut() = Vec::new();
            return;
        }
        if self.current_input_reading.borrow().as_deref() == Some(reading) {
            return;
        }
        self.set_current_input_reading(Some(reading));
        *self.fragment_cache.borrow_mut() =
            crate::lm::query_fragments_by_reading(&self.conn, reading, 16).unwrap_or_default();
    }

    /// 当前是否存在上下文（上一提交词或 surrounding_text）。
    ///
    /// 加成路径的统一门控：`lm_boost` 用它决定要不要计新增三路，
    /// `refresh_fragment_cache` 用它决定要不要查库。
    #[inline]
    fn has_context(&self) -> bool {
        self.lm_prev_text.is_some() || self.context_tail.is_some()
    }

    /// 单个字符是否为 CJK（用于判断中文上下文，避免英文污染中文 bigram）。
    fn is_cjk(c: char) -> bool {
        matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
    }

    /// 设置 LM 上下文（= 上一次提交词）：装载该词的后继 bigram 计数。
    ///
    /// 每次 learn 后由引擎调用。挖掘完成使世代号变化时自动重载缓存。
    /// 查询是一条走主键前缀（bigram 主键 prev_id 打头）的范围扫描，
    /// 个位数行，微秒级；按键热路径只查内存 HashMap。
    pub fn set_lm_context(&mut self, prev: Option<(&str, &str)>) {
        // 世代号变了（离线挖掘跑过）→ vocab id 缓存全部作废重查。
        // 与后台写线程共用同一份失效实现（#101）。
        refresh_vocab_cache(&self.conn, &mut self.lm_generation, &mut self.vocab_ids);
        let Some((text, reading)) = prev else {
            self.lm_ctx = None;
            self.lm_counts.clear();
            // 上下文断裂（外部文本/光标编辑）：实时 bigram 外键也必须清，
            // 否则下一条候选仍按上上个中文词加成。
            self.lm_prev_text = None;
            return;
        };
        // 实时 bigram 的外键 = 本次查询的「上一词」。
        self.lm_prev_text = Some(text.to_string());
        // 只读解析（#101：键路径零 SQLite 写）：vocab 行还没被后台写线程落盘时
        // miss → 本次跳过 LM 上下文，行落盘后下一次选词自愈。
        let Some(ctx) = vocab_id_readonly(&self.conn, &mut self.vocab_ids, text, reading) else {
            self.lm_ctx = None;
            self.lm_counts.clear();
            return;
        };
        self.lm_ctx = Some(ctx);
        self.lm_counts.clear();
        // JOIN vocab 把 next_id 翻译回 (text, pinyin)：候选排序按 Candidate
        // 字段直查，不要求每个候选先进过 vocab 缓存。
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT v.text, v.reading, b.count
             FROM bigram b JOIN vocab v ON v.id = b.next_id
             WHERE b.prev_id = ?1",
        ) else {
            return;
        };
        let Ok(rows) = stmt.query_map([ctx], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        }) else {
            return;
        };
        for row in rows.flatten() {
            self.lm_counts
                .entry(row.0)
                .or_default()
                .insert(row.1, row.2);
        }
    }

    /// 某候选的上下文加成。
    ///
    /// 拆成两段，因为两段的可信度与量级不同：
    ///
    /// 1. **离线 bigram**（`lm_counts`，`set_lm_context` 装载）：既有行为，
    ///    `count × LM_BOOST_UNIT` **不封顶**——这是 main 已发版的排序语义，
    ///    在候选层是主要信号之一，不能因为新增机制而被压掉。
    /// 2. **本分支新增的三路**：实时 bigram、上下文召回、离线片段。它们是
    ///    tie-breaker 性质的补充证据，合计封顶在 `CONTEXT_BOOST_CAP`：
    ///    足以让等频/近频候选中「上下文相关」者排前，又不足以压过裸频与
    ///    用户提频（`USER_BOOST = 300_000`）。
    ///
    /// 加成只落排序比较（[`crate::rank::CtxBoost`]），不碰 `eff`/导出频率。
    pub fn lm_boost(&self, c: &Candidate) -> i64 {
        // 1. 离线 bigram：既有路径，量级由 LM_BOOST_UNIT 决定，不封顶。
        let mut boost: i64 = 0;
        if !self.lm_counts.is_empty() && !c.pinyin.is_empty() {
            boost = boost.saturating_add(
                self.lm_counts
                    .get(c.text.as_str())
                    .and_then(|m| m.get(c.pinyin.as_str()))
                    .map(|&cnt| cnt.saturating_mul(crate::lm::LM_BOOST_UNIT))
                    .unwrap_or(0),
            );
        }

        // 2. 新增三路：合并后封顶，保持 tie-breaker 性质。
        //
        // 三路都**只在存在上下文时**才计分（`lm_prev_text` / `context_tail` 有值）。
        // 没有上下文 = 用户刚开始一句新输入，此时必须退回裸频序——
        // 否则「历史提交过的高频片段」会无条件压过语料频率，清上下文也不恢复
        // （`lm_mine_test::lm_context_reorders_candidates` 钉的就是这条）。
        let has_context = self.has_context();
        let mut extra: i64 = 0;
        if has_context {
            self.push_context_boost(&mut extra, c);
        }
        boost.saturating_add(extra.min(CONTEXT_BOOST_CAP))
    }

    /// 三路上下文加成之和（调用方负责「有上下文」门控与封顶）。
    fn push_context_boost(&self, extra: &mut i64, c: &Candidate) {
        // 实时 bigram：上一词（lm_prev_text）→ 本候选读音。
        // 查表方向必须是「上一词 → 候选」；按候选自己的文本查等于永远命中不了。
        if let Some(m) = self
            .lm_prev_text
            .as_deref()
            .and_then(|prev| self.realtime_bigram.get(prev))
        {
            *extra = extra.saturating_add(
                m.get(c.pinyin.as_str())
                    .copied()
                    .unwrap_or(0)
                    .saturating_mul(CONTEXT_BIGRAM_UNIT),
            );
        }
        // 上下文召回：历史提交文本与当前输入读音匹配。
        *extra = extra.saturating_add(
            self.context_recall
                .boost_for(c, self.current_input_reading.borrow().as_deref()),
        );
        // 离线片段：历史片段读音与当前输入读音匹配。
        *extra = extra.saturating_add(crate::lm::fragment_boost_for_reading(
            &self.fragment_cache.borrow(),
            self.current_input_reading.borrow().as_deref().unwrap_or(""),
            c,
        ));
        // surrounding_text 召回：候选文本出现在光标前上文里 —— 恢复「刚打过的词」。
        // 与读音类证据不同，这条按**文本**匹配，与 CJK 关键词同现即加分。
        if let Some(tail) = self.context_tail.as_deref() {
            if !tail.is_empty() && tail.contains(c.text.as_str()) {
                *extra = extra.saturating_add(CONTEXT_BIGRAM_UNIT);
            }
        }
    }
}

/// 词典作为排序内核的上下文加成来源。
///
/// 加成在这里做一次无符号饱和转换：旧实现把 `eff: u64` 直接 `as i64` 再与
/// `i64` 加成相加，超过 `i64::MAX` 时会截断成负数并反转高频段顺序。饱和到
/// `u64` 后不存在回绕，比较键再由 [`crate::rank::Ranker`] 统一饱和。
impl crate::rank::CtxBoost for Dict {
    #[inline]
    fn boost(&self, c: &Candidate) -> u64 {
        self.lm_boost(c).max(0) as u64
    }
}
