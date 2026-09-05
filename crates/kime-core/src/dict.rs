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
//! -- 索引：(pinyin)、(abbrev) —— 查询恒为 index scan + freq 排序 LIMIT n
//! ```
//!
//! 主路延迟（P99 < 20ms）由这里的索引形状保证。

use rusqlite::{params, Connection, Error as SqliteError, Result};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

fn io_to_sqlite(e: std::io::Error) -> SqliteError {
    SqliteError::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
}

/// 候选词 — 全链路统一货币：dict 查询产出、engine 排序翻页、AI 层追加
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    /// 音节 `'` 连接（"ni'hao"）— learn 回写 / 上下文线索用
    pub pinyin: String,
    pub freq: u64,
    /// true = 来自 AI 预测（UI 标注用）
    pub ai: bool,
}

/// 内存排序索引条目，按 (pinyin ASC, freq DESC, text ASC) 排序
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
}

pub struct Dict {
    conn: Connection,
    /// 内存排序索引：按 (pinyin ASC, freq DESC, text ASC) 排序
    index: Vec<IndexEntry>,
    /// 二级索引：同一批条目按 (abbrev ASC, freq DESC, text ASC) 排序——abbrev 前缀二分用
    abbrev_index: Vec<IndexEntry>,
}

/// Helper to compute exclusive upper bound for prefix range query.
/// Returns `None` when the prefix ends with `'z'` because no valid greater string
/// exists within the allowed alphabet (`'` + `a-z`).
fn increment_prefix(prefix: &str) -> Option<String> {
    let chars: Vec<char> = prefix.chars().collect();
    let last = chars.last()?;
    if *last == 'z' {
        return None;
    }
    let mut new_chars = chars.clone();
    let last_idx = new_chars.len() - 1;
    let c = new_chars[last_idx];
    if c == '\'' {
        new_chars[last_idx] = 'a';
    } else {
        let next = ((c as u32) + 1) as u32;
        new_chars[last_idx] = char::from_u32(next).unwrap_or(c);
    }
    Some(new_chars.into_iter().collect())
}

impl Dict {
    /// 打开；不存在则建 schema + 索引
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS phrase (
               pinyin  TEXT    NOT NULL,
               text    TEXT    NOT NULL,
               freq    INTEGER NOT NULL DEFAULT 0,
               abbrev  TEXT    NOT NULL,
               user    INTEGER NOT NULL DEFAULT 0,
               UNIQUE(pinyin, text)
             );
             CREATE INDEX IF NOT EXISTS idx_phrase_pinyin  ON phrase(pinyin);
             CREATE INDEX IF NOT EXISTS idx_phrase_abbrev  ON phrase(abbrev);",
        )?;
        // Load memory index from existing DB
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
            })
        })?;
        let mut index: Vec<_> = rows.collect::<Result<Vec<_>, _>>()?;
        index.sort_by(|a, b| {
            a.pinyin
                .cmp(&b.pinyin)
                .then_with(|| b.freq.cmp(&a.freq))
                .then_with(|| a.text.cmp(&b.text))
        });
        let mut abbrev_index = index.clone();
        abbrev_index.sort_by(|a, b| {
            a.abbrev
                .cmp(&b.abbrev)
                .then_with(|| b.freq.cmp(&a.freq))
                .then_with(|| a.text.cmp(&b.text))
        });
        drop(stmt);
        Ok(Self {
            conn,
            index,
            abbrev_index,
        })
    }

    /// 导入 rime-ice `.dict.yaml`：解析 TSV 正文（文字\t拼音\t频率），
    pub fn import(&mut self, dict_yaml: impl AsRef<Path>) -> Result<usize> {
        let f = File::open(dict_yaml).map_err(io_to_sqlite)?;
        let reader = BufReader::new(f);
        // Skip YAML front-matter
        let mut lines = reader.lines().map_while(Result::ok);
        for line in &mut lines {
            if line.trim() == "..." {
                break;
            }
        }
        let tx = self.conn.transaction()?;
        let mut count = 0usize;
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let mut cols = line.split('\t');
            let text = match cols.next() {
                Some(t) if !t.is_empty() => t,
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
            let syllables: Vec<&str> = pinyin.split_whitespace().collect();
            let joined = syllables.join("'");
            let abbrev: String = syllables.iter().filter_map(|s| s.chars().next()).collect();
            let abbrev = abbrev.to_lowercase();
            let added = tx.execute(
                "INSERT OR IGNORE INTO phrase(pinyin, text, freq, abbrev, user) VALUES (?1, ?2, ?3, ?4, 0)",
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
                user: row.get(4)?,
            })
        })?;
        let mut index: Vec<_> = rows.collect::<Result<Vec<_>, _>>()?;
        index.sort_by(|a, b| {
            a.pinyin
                .cmp(&b.pinyin)
                .then_with(|| b.freq.cmp(&a.freq))
                .then_with(|| a.text.cmp(&b.text))
        });
        self.index = index;
        let mut abbrev_index = self.index.clone();
        abbrev_index.sort_by(|a, b| {
            a.abbrev
                .cmp(&b.abbrev)
                .then_with(|| b.freq.cmp(&a.freq))
                .then_with(|| a.text.cmp(&b.text))
        });
        self.abbrev_index = abbrev_index;
        Ok(count)
    }

    /// 精确查询：读音序列 → 候选，freq 降序 LIMIT limit
    pub fn lookup(&self, reading: &[String], limit: usize) -> Result<Vec<Candidate>> {
        if reading.is_empty() {
            return Ok(Vec::new());
        }
        let joined = reading.join("'");
        // 等值区间：pinyin 相同的条目在索引中连续；partition_point 定位起点
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
                ai: false,
            });
        }
        candidates.sort_by(|a, b| b.freq.cmp(&a.freq).then_with(|| a.text.cmp(&b.text)));
        candidates.truncate(limit);
        Ok(candidates)
    }

    /// 前缀查询：完整音节 `syllables` 后面接未完成的 `tail`，返回 freq 降序前 N 候选。
    ///
    /// 拼出模式串 = `syllables.join("'")` (+ `'` + tail 当 tail 非空)。
    /// LIKE 匹配以此开头的 pinyin，附加 `%` 通配后续音节。
    /// 模式只含 `a-z` 与 `'`；LIKE 通配符仅出现在尾部，无 escape 风险。
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
        if joined.is_empty() {
            return Ok(Vec::new());
        }
        let upper_bound = increment_prefix(&joined).unwrap_or_else(|| joined.clone());
        // Find range [lower, upper)
        let lower_idx = self.index.partition_point(|e| e.pinyin < joined);
        let upper_idx = self.index.partition_point(|e| e.pinyin < upper_bound);
        let slice = &self.index[lower_idx..upper_idx];
        let mut candidates: Vec<Candidate> = slice
            .iter()
            .map(|e| Candidate {
                text: e.text.clone(),
                pinyin: e.pinyin.clone(),
                freq: e.freq as u64,
                ai: false,
            })
            .collect();
        candidates.sort_by(|a, b| b.freq.cmp(&a.freq).then_with(|| a.text.cmp(&b.text)));
        candidates.truncate(limit);
        Ok(candidates)
    }

    /// 缩写前缀查询：`initials` 是声母序列（"nh"），匹配所有以该串开头的 abbrev。
    /// 校验：initials 仅允许 a-z；空串 → 空 vec。
    /// 排序 freq DESC, text ASC，LIMIT 限条。
    pub fn lookup_abbrev(&self, initials: &str, limit: usize) -> Result<Vec<Candidate>> {
        if initials.is_empty() {
            return Ok(Vec::new());
        }
        if !initials.bytes().all(|b| b.is_ascii_lowercase()) {
            return Ok(Vec::new());
        }
        // 二级索引按 (abbrev ASC, freq DESC, text ASC) 排序——二分定位前缀区间。
        // abbrev 是纯 a-z，上界 = 前缀末字符 +1（z 由调用方短路）。
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
                ai: false,
            })
            .collect();
        candidates.truncate(limit);
        Ok(candidates)
    }

    /// 用户词前 N：user=1 行按 freq DESC, text ASC（engine 个性化排序用）。
    pub fn top_user(&self, limit: usize) -> Result<Vec<Candidate>> {
        let mut candidates: Vec<Candidate> = self
            .index
            .iter()
            .filter(|e| e.user == 1)
            .map(|e| Candidate {
                text: e.text.clone(),
                pinyin: e.pinyin.clone(),
                freq: e.freq as u64,
                ai: false,
            })
            .collect();
        // Apply freq DESC, text ASC ordering and limit
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
        if let Some(entry) = self
            .index
            .iter_mut()
            .find(|e| e.pinyin == joined && e.text == text)
        {
            self.conn.execute(
                "UPDATE phrase SET freq = freq + 1, user = 1 WHERE pinyin = ?1 AND text = ?2",
                params![joined, text],
            )?;
            entry.freq += 1;
            entry.user = 1;
            return Ok(());
        }
        // Insert new user word
        self.conn.execute(
            "INSERT INTO phrase(pinyin, text, freq, abbrev, user) VALUES (?1, ?2, 1, ?3, 1)",
            params![joined, text, abbrev],
        )?;
        // Insert into memory index and keep sorted
        let new_entry = IndexEntry {
            pinyin: joined,
            text: text.to_string(),
            freq: 1,
            abbrev,
            user: 1,
        };
        let pos = self.index.partition_point(|e| e.pinyin < new_entry.pinyin);
        self.index.insert(pos, new_entry.clone());
        let apos = self
            .abbrev_index
            .partition_point(|e| e.abbrev < new_entry.abbrev);
        self.abbrev_index.insert(apos, new_entry);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
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

    #[test]
    fn lookup_prefix_empty_tail_equals_exact_lookup() {
        let yaml = seed_yaml("...\n你好\tni hao\t5000\n我们\two men\t4000\n");
        let db = tmp_db("seed");
        let mut d = Dict::open(&db).unwrap();
        d.import(&yaml).unwrap();
        let exact = d.lookup(&["ni".into(), "hao".into()], 10).unwrap();
        let prefix = d
            .lookup_prefix(&["ni".into(), "hao".into()], "", 10)
            .unwrap();
        assert_eq!(exact.len(), prefix.len());
        assert_eq!(exact.len(), 1);
        assert_eq!(prefix[0].text, "你好");
        assert_eq!(prefix[0].pinyin, "ni'hao");
        assert_eq!(prefix[0].freq, 5000);
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
}
