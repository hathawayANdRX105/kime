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

use rusqlite::{params, Connection, Error as SqliteError, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

fn io_to_sqlite(e: std::io::Error) -> SqliteError {
    SqliteError::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
}
/// 用户词提频（第五轮调频方案 B：使用即提升 + 时间衰减，rime user_freq 同款语义）。
///
/// 语料计数是 10⁵~10⁷ 量级（常见词「我们」freq=509,405），旧版 learn 一次 +1
/// 对排序毫无作用。第 n 次使用给该词叠加 `n × USER_BOOST × 0.5^(age/半衰期)`
/// 的**排序用**有效频率。首用即满额是用户拍板（2026-09-15）：rime 手感——
/// 选过的词立刻要有存在感；`tests/dict.rs` 钉「等量加成相抵、排序仍由裸频定」。
///
/// 校准：BOOST=300_000 ⇒ 第 1 次使用（300k）压过 10⁵ 级长尾词、压不过 50 万级
/// 语料词；第 2 次（600k）压过 freq=500,000 的普通语料词。
const USER_BOOST: u64 = 300_000;
/// 提频半衰期（天）：连续 30 天不再使用，加成减半；60 天降到 1/4，回落语料位。
const USER_BOOST_HALF_LIFE_DAYS: f64 = 30.0;

/// 「今天」= UNIX 纪元以来的天数。衰减窗口按天推进即可（半衰期本身 30 天粒度）。
fn today_days() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0)
}

/// 某词的提频加成。`stats[(pinyin, text)] = (使用次数 n, 最近使用日)`；
/// 无记录 → 0；n=1 即满额（首用即加成，用户拍板 2026-09-15）。纯函数（除查表），
/// learn/开库共用同一条算式，无第二份语义。
fn user_bonus_of(
    stats: &HashMap<(String, String), (u64, u64)>,
    today: u64,
    pinyin: &str,
    text: &str,
) -> u64 {
    let Some(&(n, last)) = stats.get(&(pinyin.to_string(), text.to_string())) else {
        return 0;
    };
    let age = today.saturating_sub(last) as f64;
    let factor = 0.5f64.powf(age / USER_BOOST_HALF_LIFE_DAYS);
    (n as f64 * USER_BOOST as f64 * factor) as u64
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
/// 候选词 — 全链路统一货币：dict 查询产出、engine 排序翻页、AI 层追加
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    /// 音节 `'` 连接（"ni'hao"）— learn 回写 / 上下文线索用
    pub pinyin: String,
    pub freq: u64,
    /// 排序用有效频率（语料裸频 + 用户提频加成，开库/learn 时烘焙）。
    /// `freq` 恒为库内原始值（导出契约），加成只落在这里。
    pub eff: u64,
    /// true = 来自 AI 预测（UI 标注用）
    pub ai: bool,
}

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
struct EnglishEntry {
    key: String,
    text: String,
    freq: i64,
}

pub struct Dict {
    conn: Connection,
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
    /// 用户使用计数：(pinyin, text) → (累计使用次数 n, 最近使用日)。
    /// 语料计数与用户计数分开的「独立空间」：phrase.freq 不动，排序加成
    /// 由 (n, 日期) 现算（见 `user_bonus_of`）。持久化在无 schema 迁移的
    /// `kime_kv(key, value)` 表——选它而不是把计数压进 freq 高位：后者会污染
    /// 所有读 freq 的路径（builder、overlay 权威读回、memory_vs_sql 一致性），
    /// 前者只有这一张几十行的旁表，learn 一次 UPSERT，开库一次全量 SELECT。
    user_stats: HashMap<(String, String), (u64, u64)>,
    /// 有提频记录的拼音集合（user_stats 键的拼音投影）。查询时 O(1) 判断
    /// 「该拼音块是否可能含提频行」——只有打学到过的拼音才触发块内 eff 重排。
    user_pinyins: std::collections::HashSet<String>,
    /// `today_days()` 在开库/learn 时刷新的缓存值（比较器热路径不碰系统时钟）。
    today: u64,
    /// 词表由本进程自己写入，进程内无外部并发写入者，缓存不会脏。
    vocab_ids: HashMap<(String, String), i64>,
    /// LM 上下文：上次提交词的 vocab id（None = 无上下文，不加成）。
    lm_ctx: Option<i64>,
    /// lm_ctx 的后继 bigram 计数（next_id → count），set_lm_context 时一次装载。
    lm_counts: HashMap<i64, i64>,
    /// 已消费的挖掘世代号（kime_kv 'lm_generation'）：变化即重载 lm_counts。
    lm_generation: i64,
}

/// Helper to compute exclusive upper bound for prefix range query（与 `store::increment_prefix`
/// 同语义：末字符 +1，`'z'` 进 `'{'`——只对小写字母串求上界，`'` 结尾进 `'a'`。
/// 旧版对 `'z'` 返回 None，SQLite 回退路径据此把整个补全区间塌缩成空集，与 store 路径
/// 行为不一致；None 现在只可能出现在空串上，而空 joined 在查询入口就被挡掉。）
fn increment_prefix(prefix: &str) -> Option<String> {
    let mut chars: Vec<char> = prefix.chars().collect();
    let last_idx = chars.len().checked_sub(1)?;
    let c = chars[last_idx];
    chars[last_idx] = if c == '\'' {
        'a'
    } else {
        char::from_u32(c as u32 + 1).unwrap_or(c)
    };
    Some(chars.into_iter().collect())
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
               ts      INTEGER NOT NULL,
               ctx_id  INTEGER,
               text_id INTEGER NOT NULL,
               reading TEXT    NOT NULL
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
             ",
        )?;
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
                    eprintln!(
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
        // 有提频记录的拼音集合：查询时判断「该块是否需要按 eff 重排」的 O(1) 依据
        //（集合是个位到几百条，层一块内重排只在打学到过的拼音时发生）。
        let user_pinyins: std::collections::HashSet<String> =
            user_stats.keys().map(|(p, _)| p.clone()).collect();
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
        let english = Self::load_english(&conn)?;
        Ok(Self {
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
        })
    }

    /// 英文表全量入内存。排序交给 SQL：`lower(text)` 就是匹配键（text 恒为 ASCII，
    /// SQLite 的 `lower()` 只处理 ASCII，正好是我们要的语义），省掉 Rust 端一次重排。
    /// 表空/不存在时返回空 Vec，开销是一次 prepare。
    fn load_english(conn: &Connection) -> Result<Vec<EnglishEntry>> {
        let mut stmt = conn
            .prepare("SELECT lower(text), text, freq FROM english ORDER BY 1 ASC, 3 DESC, 2 ASC")?;
        let rows = stmt.query_map(params![], |row| {
            Ok(EnglishEntry {
                key: row.get(0)?,
                text: row.get(1)?,
                freq: row.get(2)?,
            })
        })?;
        rows.collect()
    }

    /// 英文词查询：拿**原始按键串**匹配，不做拼音解码（对齐 rime 的 english translator）。
    ///
    /// 两层契约与中文 `lookup_prefix` 一致：层一 = key 恰等于输入的条目，层二 = 以输入为
    /// 前缀的补全；**每层各自**按 `(freq DESC, text ASC)`，层一整体在前，绝不跨层打平——
    /// 否则打 `help` 会被 `helper` 顶掉首候选。层二必须重排：内存 Vec 的主序是 key 升序，
    /// 一段前缀区间里跨了多个 key，天然不是频率序（中文那条路径同理）。
    /// 输入大小写不敏感，上屏文本保持词库原写法（`AA` 与 `aa` 是两条独立条目，都能被 `aa` 命中）。
    pub fn lookup_english(&self, raw: &str, limit: usize) -> Vec<Candidate> {
        if raw.is_empty() || limit == 0 {
            return Vec::new();
        }
        let key = raw.to_ascii_lowercase();
        let start = self
            .english
            .partition_point(|e| e.key.as_str() < key.as_str());
        let exact_len = self.english[start..]
            .iter()
            .take_while(|e| e.key == key)
            .count();
        let mut out: Vec<Candidate> = self.english[start..start + exact_len]
            .iter()
            .map(Self::english_to_candidate)
            .take(limit)
            .collect();
        let mut comps: Vec<Candidate> = self.english[start + exact_len..]
            .iter()
            .take_while(|e| e.key.starts_with(&key))
            .map(Self::english_to_candidate)
            .collect();
        // ponytail: 整段重排（最坏 `a` 命中 ~4k 条，release 下几 µs）；哪天真嫌慢再换 top-k 堆。
        comps.sort_by(|a, b| b.freq.cmp(&a.freq).then_with(|| a.text.cmp(&b.text)));
        comps.truncate(limit - out.len());
        out.extend(comps);
        out
    }

    fn english_to_candidate(e: &EnglishEntry) -> Candidate {
        Candidate {
            text: e.text.clone(),
            // pinyin 填词本身：主控的分层判断靠它区分「消耗完输入」与「补全」
            pinyin: e.text.clone(),
            freq: e.freq.max(0) as u64,
            eff: e.freq.max(0) as u64,
            ai: false,
        }
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
    fn load_user_stats(conn: &Connection) -> Result<HashMap<(String, String), (u64, u64)>> {
        let mut stmt = conn.prepare("SELECT key, value FROM kime_kv")?;
        let rows = stmt.query_map(params![], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = HashMap::new();
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
            out.insert((pinyin.to_string(), text.to_string()), (n, day));
        }
        Ok(out)
    }

    /// 候选的排序用有效频率。eff 在条目构造/learn 时烘焙（含用户提频加成），
    /// `Candidate::freq` 本身恒为库内原始值——加成不改导出值。
    pub fn effective_freq(&self, c: &Candidate) -> u64 {
        c.eff
    }

    /// (有效频率 + LM 上下文加成 DESC, text ASC)——候选层的统一排序键。
    /// eff 是烘焙好的字段（零哈希零 powf）；lm_boost 是两次内存 HashMap 查找，
    /// 无上下文/空计数时 O(1) 短路为 0，排序退化为纯 eff。
    fn cand_cmp(&self, a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
        let ka = a.eff as i64 + self.lm_boost(a);
        let kb = b.eff as i64 + self.lm_boost(b);
        kb.cmp(&ka).then_with(|| a.text.cmp(&b.text))
    }

    /// 语料总词频。见字段注释：为什么在开库时算。
    pub fn total_freq(&self) -> u64 {
        self.total_freq
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
        self.english = Self::load_english(&self.conn)?;
        Ok(count)
    }

    /// 用户词 overlay：精确 pinyin 命中。`user_overlay` 按 pinyin ASC，二分定位后线性取等值段。
    fn overlay_exact(&self, joined: &str) -> Vec<Candidate> {
        let start = self
            .user_overlay
            .partition_point(|e| e.pinyin.as_str() < joined);
        self.user_overlay[start..]
            .iter()
            .take_while(|e| e.pinyin == joined)
            .map(Self::entry_to_candidate)
            .collect()
    }

    /// 用户词 overlay：pinyin 前缀区间 `[lower, upper)`。`upper` 为 None 表示无上界（前缀以 z 结尾）。
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
        if tail.is_empty() {
            let lower = format!("{joined}'");
            let upper = increment_prefix(&lower);
            self.overlay_prefix(&lower, upper.as_deref())
        } else {
            let upper = increment_prefix(joined);
            let mut v = self.overlay_prefix(joined, upper.as_deref());
            v.retain(|c| c.pinyin != joined);
            v
        }
    }

    /// 用户词 overlay：abbrev 前缀命中。overlay 按 pinyin 排序，abbrev 只能全扫——
    /// 集合是个位到几十条，扫一遍比维护第二份排序便宜。
    fn overlay_abbrev(&self, initials: &str) -> Vec<Candidate> {
        self.user_overlay
            .iter()
            .filter(|e| e.abbrev.starts_with(initials))
            .map(Self::entry_to_candidate)
            .collect()
    }

    fn entry_to_candidate(e: &IndexEntry) -> Candidate {
        Candidate {
            text: e.text.clone(),
            pinyin: e.pinyin.clone(),
            freq: e.freq as u64,
            eff: e.eff,
            ai: false,
        }
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
        let mut merged: HashMap<String, Candidate> = HashMap::new();
        for cand in base {
            merged.entry(cand.text.clone()).or_insert(cand);
        }
        for cand in user {
            merged.insert(cand.text.clone(), cand);
        }
        let mut candidates: Vec<Candidate> = merged.into_values().collect();
        candidates.sort_by(|a, b| self.cand_cmp(a, b));
        candidates.truncate(limit);
        candidates
    }

    /// 精确查询：读音序列 → 候选，freq 降序 LIMIT limit
    pub fn lookup(&self, reading: &[String], limit: usize) -> Result<Vec<Candidate>> {
        if reading.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(store) = &self.store {
            let base = store.lookup_exact(reading, limit);
            let joined = reading.join("'");
            let user_candidates = self.overlay_exact(&joined);
            return Ok(self.merge_overlay(base, user_candidates, limit));
        }
        let joined = reading.join("'");
        let start = self.index.partition_point(|e| e.pinyin < joined);
        let mut candidates = Vec::new();
        for entry in &self.index[start..] {
            if entry.pinyin != joined {
                break;
            }
            candidates.push(Candidate {
                text: entry.text.clone(),
                pinyin: entry.pinyin.clone(),
                freq: entry.freq as u64,
                eff: entry.eff,
                ai: false,
            });
        }
        candidates.sort_by(|a, b| self.cand_cmp(a, b));
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
        let user_exact = self.overlay_exact(&joined);
        let mut user_comps = self.overlay_comps(&joined, tail);

        if let Some(store) = &self.store {
            let (mut base_exact, mut base_comps) =
                store.lookup_prefix_layers(syllables, tail, limit);
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
                base_exact.sort_by(|a, b| self.cand_cmp(a, b));
            }
            let l1_cap = if tail.is_empty() {
                limit
            } else {
                limit.div_ceil(2)
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
            limit.div_ceil(2)
        };
        let mut out: Vec<Candidate> = if self.user_pinyins.contains(joined.as_str()) {
            let mut block: Vec<Candidate> = self.index[start..start + exact_len]
                .iter()
                .map(Self::entry_to_candidate)
                .collect();
            block.sort_by(|a, b| self.cand_cmp(a, b));
            block.truncate(cap);
            block
        } else {
            self.index[start..]
                .iter()
                .take_while(|e| e.pinyin == joined)
                .map(Self::entry_to_candidate)
                .take(cap)
                .collect()
        };
        let (lo, hi) = if tail.is_empty() {
            let lower = format!("{joined}'");
            let upper = increment_prefix(&lower).unwrap_or_else(|| lower.clone());
            (
                self.index.partition_point(|e| e.pinyin < lower),
                self.index.partition_point(|e| e.pinyin < upper),
            )
        } else {
            let upper = increment_prefix(&joined).unwrap_or_else(|| joined.clone());
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
            out.sort_by(|a, b| self.cand_cmp(a, b));
        }
        let mut comps: Vec<Candidate> = self.index[lo..hi]
            .iter()
            .map(Self::entry_to_candidate)
            .filter(|c| !out.iter().any(|o| o.text == c.text))
            .collect();
        comps.sort_by(|a, b| self.cand_cmp(a, b));
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
                .map(|e| Candidate {
                    text: e.text.clone(),
                    pinyin: e.pinyin.clone(),
                    freq: e.freq as u64,
                    eff: e.eff,
                    ai: false,
                })
                .collect();
            // 与 FST 分支同语义：缩写查询也按有效频率（裸频 + 提频加成）排序，
            // 否则用户打缩写时「使用频率高的在前」不生效，两条路径行为分裂。
            candidates.sort_by(|a, b| self.cand_cmp(a, b));
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

    /// 学习：更新词频或插入用户词。
    pub fn learn(&mut self, reading: &[String], text: &str) -> Result<()> {
        if reading.is_empty() {
            return Ok(());
        }
        let joined = reading.join("'");
        let abbrev: String = reading.iter().filter_map(|s| s.chars().next()).collect();
        let abbrev = abbrev.to_lowercase();
        let updated = self.conn.execute(
            "UPDATE phrase SET freq = freq + 1, user = 1 WHERE pinyin = ?1 AND text = ?2",
            params![joined, text],
        )?;
        if updated == 0 {
            self.conn.execute(
                "INSERT INTO phrase(pinyin, text, freq, abbrev, user) VALUES (?1, ?2, 1, ?3, 1)",
                params![joined, text, abbrev],
            )?;
        }

        // 用户提频计数（方案 B：使用即提升 + 按天时间衰减，rime user_freq 语义）。
        // phrase.freq 保持旧语义（词库频率 + 累计 bump 次数），使用次数 n 与最近使用日
        // 写旁表 kime_kv；加成只进比较器，不改任何导出频率。首用即满额
        // （n×300k，用户拍板 2026-09-15）——选过的词立刻要有存在感；
        // `tests/dict.rs` 钉「等量加成相抵、排序仍由裸频定」。
        self.today = today_days();
        let stats_key = (joined.clone(), text.to_string());
        let n = self
            .user_stats
            .get(&stats_key)
            .map(|(n, _)| n + 1)
            .unwrap_or(1);
        self.user_stats.insert(stats_key.clone(), (n, self.today));
        self.conn.execute(
            "INSERT INTO kime_kv(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![
                format!("{}\t{}", joined, text),
                format!("{n},{}", self.today)
            ],
        )?;

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
                    // 这行刚从词库词转成用户词（UPDATE 命中，user 0→1），它的真实频率是
                    // 词库频率 + 1，不是 1。读回权威值，别让 overlay 把候选踢到末尾。
                    let freq: i64 = self.conn.query_row(
                        "SELECT freq FROM phrase WHERE pinyin = ?1 AND text = ?2",
                        params![&joined, text],
                        |row| row.get(0),
                    )?;
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
                pinyin: joined,
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
        let lo = self
            .index
            .partition_point(|e| e.pinyin.as_str() < stats_key.0.as_str());
        let hi = self
            .index
            .partition_point(|e| e.pinyin.as_str() <= stats_key.0.as_str());
        self.index[lo..hi].sort_by(entry_cmp);
        Ok(())
    }

    /// todo/2026-09-18-offline-lm-design.md 第 0 层）。
    ///
    /// `ctx` = 上一次 kime 上屏的词 `(text, reading)`，用来挖掘 bigram。
    /// 用引擎自记的上次提交而非解析 surrounding_text：后者混着非 kime 输入的文本
    /// （粘贴、kime 启动前打的字），bigram 会被污染；前者恒为 kime 自己的提交。
    ///
    /// 失败**不阻塞上屏**：commit_log 缺失只影响离线排序质量，本次输入照常完成。
    pub fn log_commit(&mut self, ctx: Option<(&str, &str)>, reading: &[String], text: &str) {
        if reading.is_empty() || text.is_empty() {
            return;
        }
        let joined = reading.join("'");
        let today = today_days();
        let text_id = match self.vocab_id(text, &joined, today) {
            Some(id) => id,
            None => return,
        };
        let ctx_id = ctx.and_then(|(t, r)| self.vocab_id(t, r, today));
        let _ = self.conn.execute(
            "INSERT INTO commit_log(ts, ctx_id, text_id, reading) VALUES (?1, ?2, ?3, ?4)",
            params![today as i64, ctx_id, text_id, joined],
        );
    }

    /// 设置 LM 上下文（= 上一次提交词）：装载该词的后继 bigram 计数。
    ///
    /// 每次 learn 后由引擎调用。挖掘完成使世代号变化时自动重载缓存。
    /// 查询是一条走主键前缀（bigram 主键 prev_id 打头）的范围扫描，
    /// 个位数行，微秒级；按键热路径只查内存 HashMap。
    pub fn set_lm_context(&mut self, prev: Option<(&str, &str)>) {
        // 世代号变了（离线挖掘跑过）→ vocab id 缓存全部作废重查
        let gen: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(CAST(value AS INTEGER), 0) FROM kime_kv
                 WHERE key = 'lm_generation'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if gen != self.lm_generation {
            self.lm_generation = gen;
            self.vocab_ids.clear();
        }
        let Some((text, reading)) = prev else {
            self.lm_ctx = None;
            self.lm_counts.clear();
            return;
        };
        let Some(ctx) = self.vocab_id(text, reading, today_days()) else {
            self.lm_ctx = None;
            self.lm_counts.clear();
            return;
        };
        self.lm_ctx = Some(ctx);
        self.lm_counts.clear();
        let Ok(mut stmt) = self
            .conn
            .prepare("SELECT next_id, count FROM bigram WHERE prev_id = ?1")
        else {
            return;
        };
        let Ok(rows) = stmt.query_map([ctx], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
        else {
            return;
        };
        for row in rows.flatten() {
            self.lm_counts.insert(row.0, row.1);
        }
    }

    /// 某 (text, reading) 的 LM 加成：上次提交词的后继计数 × LM_BOOST_UNIT。
    /// 加成直接落排序比较，不碰 eff/导出频率。
    /// 热路径：两次内存 HashMap 查找（vocab id 缓存 + counts）。
    pub fn lm_boost(&self, c: &Candidate) -> i64 {
        if self.lm_counts.is_empty() || c.pinyin.is_empty() {
            return 0;
        }
        self.vocab_ids
            .get(&(c.text.clone(), c.pinyin.clone()))
            .and_then(|id| self.lm_counts.get(id))
            .map(|&cnt| cnt * crate::lm::LM_BOOST_UNIT)
            .unwrap_or(0)
    }

    /// 取（必要时建）vocab 行的 id。`log_commit` 每次提交都调，命中缓存时零 SQL。
    fn vocab_id(&mut self, text: &str, reading: &str, today: u64) -> Option<i64> {
        let key = (text.to_string(), reading.to_string());
        if let Some(&id) = self.vocab_ids.get(&key) {
            return Some(id);
        }
        // SELECT 命中 → 缓存；未命中 → 插入（UNIQUE 冲突说明并发已建，忽略）
        // 再回查拿 id。不使用 RETURNING：旧 SQLite 不支持，且 execute 不返回行。
        let id = self
            .conn
            .query_row(
                "SELECT id FROM vocab WHERE text = ?1 AND reading = ?2",
                params![text, reading],
                |row| row.get(0),
            )
            .ok()
            .or_else(|| {
                let _ = self.conn.execute(
                    "INSERT OR IGNORE INTO vocab(text, reading, last_seen) VALUES (?1, ?2, ?3)",
                    params![text, reading, today as i64],
                );
                self.conn
                    .query_row(
                        "SELECT id FROM vocab WHERE text = ?1 AND reading = ?2",
                        params![text, reading],
                        |row| row.get(0),
                    )
                    .ok()
            });
        if let Some(id) = id {
            self.vocab_ids.insert(key, id);
        }
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_db(suffix: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "kime_dict_lp_{}_{}_{}.sqlite",
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

    fn seed_yaml(content: &str) -> std::path::PathBuf {
        let yaml = std::env::temp_dir().join(format!(
            "kime_dict_yaml_{}_{}.yaml",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&yaml, content).unwrap();
        yaml
    }

    /// `lookup` 是精确查询：只返回该读音自己的词。
    /// （曾用 `lookup_prefix(.., "", ..)` 实现，那是前缀区间，见
    /// `tests/lookup_exact_test.rs`。）
    #[test]
    fn lookup_returns_only_the_exact_reading() {
        let yaml = seed_yaml("...\n你好\tni hao\t5000\n我们\two men\t4000\n");
        let db = tmp_db("seed");
        let mut d = Dict::open(&db).unwrap();
        d.import(&yaml).unwrap();
        let exact = d.lookup(&["ni".into(), "hao".into()], 10).unwrap();
        assert_eq!(exact.len(), 1, "另一读音的词不应混入");
        assert_eq!(exact[0].text, "你好");
        assert_eq!(exact[0].pinyin, "ni'hao");
        assert_eq!(exact[0].freq, 5000);
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn lookup_prefix_partial_tail_returns_prefix_matches() {
        let yaml = seed_yaml("...\n你好\tni hao\t5000\n泥猴\tni hou\t100\n");
        let db = tmp_db("seed");
        let mut d = Dict::open(&db).unwrap();
        d.import(&yaml).unwrap();
        let hits = d.lookup_prefix(&["ni".into()], "h", 10).unwrap();
        let texts: Vec<&str> = hits.iter().map(|c| c.text.as_str()).collect();
        assert!(texts.contains(&"你好"));
        assert!(texts.contains(&"泥猴"));
        let hits2 = d.lookup_prefix(&["ni".into()], "ha", 10).unwrap();
        assert_eq!(hits2.len(), 1);
        assert_eq!(hits2[0].text, "你好");
        let hits3 = d.lookup_prefix(&["ni".into()], "z", 10).unwrap();
        assert!(hits3.is_empty());
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn lookup_prefix_respects_limit() {
        let yaml = seed_yaml("...\nA\tha\t50\nB\tha\t40\nC\tha\t30\n");
        let db = tmp_db("seed");
        let mut d = Dict::open(&db).unwrap();
        d.import(&yaml).unwrap();
        let all = d.lookup_prefix(&["ha".into()], "", 10).unwrap();
        assert_eq!(all.len(), 3);
        let two = d.lookup_prefix(&["ha".into()], "", 2).unwrap();
        assert_eq!(two.len(), 2);
        assert_eq!(two[0].text, "A");
        assert_eq!(two[1].text, "B");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn lookup_abbrev_prefix() {
        let yaml = seed_yaml("...\n你好\tni hao\t5000\nabc\tABC\t100\n");
        let db = tmp_db("seed");
        let mut d = Dict::open(&db).unwrap();
        d.import(&yaml).unwrap();
        let nh = d.lookup_abbrev("nh", 10).unwrap();
        assert_eq!(nh.len(), 1);
        assert_eq!(nh[0].text, "你好");
        let a = d.lookup_abbrev("a", 10).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].text, "abc");
        let z = d.lookup_abbrev("z", 10).unwrap();
        assert!(z.is_empty());
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn top_user_works() {
        let yaml = seed_yaml("...\n你好\tni hao\t5000\n");
        let db = tmp_db("seed");
        let mut d = Dict::open(&db).unwrap();
        d.import(&yaml).unwrap();
        d.learn(&["ni".into(), "hao".into()], "你好").unwrap();
        d.learn(&["ni".into(), "hao".into()], "你好").unwrap();
        let top = d.top_user(5).unwrap();
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].text, "你好");
        assert_eq!(top[0].freq, 5002);
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn learn_immediate_reflection() {
        let db = tmp_db("learn");
        let mut d = Dict::open(&db).unwrap();
        d.learn(&["ni".into(), "hao".into()], "你好").unwrap();
        let cand = d.lookup(&["ni".into(), "hao".into()], 1).unwrap();
        assert_eq!(cand.len(), 1);
        assert_eq!(cand[0].text, "你好");
        assert_eq!(cand[0].freq, 1);
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn import_rebuild_keeps_order() {
        let db = tmp_db("import");
        let yaml = seed_yaml("...\nZ\tZ\t100\nA\tA\t200\n");
        let mut d = Dict::open(&db).unwrap();
        d.import(&yaml).unwrap();
        let sorted = d
            .index
            .iter()
            .map(|e| (e.pinyin.clone(), e.freq))
            .collect::<Vec<_>>();
        assert_eq!(sorted[0], ("A".to_string(), 200));
        assert_eq!(sorted[1], ("Z".to_string(), 100));
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn memory_vs_sql_consistency() {
        let db = tmp_db("consistency");
        let yaml = seed_yaml("...\n你好\tni hao\t5\n泥猴\tni hou\t3\n");
        let mut d = Dict::open(&db).unwrap();
        d.import(&yaml).unwrap();
        d.learn(&["ni".into(), "hao".into()], "你好").unwrap();
        d.learn(&["ni".into(), "hao".into()], "你好").unwrap();
        d.learn(&["ni".into(), "hou".into()], "泥猴").unwrap();
        let conn2 = Connection::open(&db).unwrap();
        let mut stmt = conn2
            .prepare(
                "SELECT text, pinyin, freq FROM phrase ORDER BY pinyin ASC, freq DESC, text ASC",
            )
            .unwrap();
        let sql_rows: Vec<_> = stmt
            .query_map(params![], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let mem_rows: Vec<_> = d
            .index
            .iter()
            .map(|e| (e.text.clone(), e.pinyin.clone(), e.freq))
            .collect();
        assert_eq!(sql_rows.len(), mem_rows.len());
        for ((sql_text, sql_pinyin, sql_freq), (mem_text, mem_pinyin, mem_freq)) in
            sql_rows.iter().zip(mem_rows.iter())
        {
            assert_eq!(sql_text, mem_text);
            assert_eq!(sql_pinyin, mem_pinyin);
            assert_eq!(sql_freq, mem_freq);
        }
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn test_fst_store_composite_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("dict.sqlite3");
        let bin_path = dir.path().join("dict.bin");
        let yaml_path = dir.path().join("test.yaml");

        // 1. 初始化 SQLite 词库并导入基础数据
        fs::write(&yaml_path, "...\n你好\tni hao\t100\n拟好\tni hao\t50\n").unwrap();
        let mut seed_dict = Dict::open(&db_path).unwrap();
        seed_dict.import(&yaml_path).unwrap();
        drop(seed_dict);

        // 2. 编译出 dict.bin
        let count = crate::builder::build(&db_path, &bin_path).unwrap();
        assert_eq!(count, 2);

        // 3. 打开复合 Dict，此时应该自动发现并挂载 FST
        let mut dict = Dict::open(&db_path).unwrap();
        assert!(dict.store.is_some(), "应当成功加载 FST store");

        // 初始查询：你好 (100) > 拟好 (50)
        let init_hits = dict.lookup_prefix(&["ni".into()], "hao", 10).unwrap();
        assert_eq!(init_hits.len(), 2);
        assert_eq!(init_hits[0].text, "你好");

        // 4. 用户学习：将拟好调频到高频，并新增未录入生词“妮好”
        for _ in 0..200 {
            dict.learn(&["ni".into(), "hao".into()], "拟好").unwrap();
        }
        dict.learn(&["ni".into(), "hao".into()], "妮好").unwrap();

        // 5. 复合查询：拟好被用户高频置顶，妮好被作为新词查出
        let updated_hits = dict.lookup_prefix(&["ni".into()], "hao", 10).unwrap();
        assert_eq!(updated_hits.len(), 3);
        assert_eq!(updated_hits[0].text, "拟好");
        assert!(updated_hits.iter().any(|c| c.text == "妮好"));

        // 6. lookup 精确查询也支持覆盖
        let exact = dict.lookup(&["ni".into(), "hao".into()], 10).unwrap();
        assert_eq!(exact[0].text, "拟好");
    }
}
