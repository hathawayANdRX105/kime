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

pub struct Dict {
    conn: Connection,
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
        Ok(Self { conn })
    }

    /// 导入 rime-ice `.dict.yaml`：解析 TSV 正文（文字\t拼音\t频率），
    pub fn import(&mut self, dict_yaml: impl AsRef<Path>) -> Result<usize> {
        let f = File::open(dict_yaml).map_err(io_to_sqlite)?;

        let reader = BufReader::new(f);

        // Skip YAML front-matter: everything before (and including) the first "..." line.
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

            // join syllables with apostrophe, abbrev is initials

            let syllables: Vec<&str> = pinyin.split_whitespace().collect();
            let joined = syllables.join("'");
            let abbrev: String = syllables.iter().filter_map(|s| s.chars().next()).collect();

            let added = tx.execute(
                "INSERT OR IGNORE INTO phrase(pinyin, text, freq, abbrev, user)
                 VALUES (?1, ?2, ?3, ?4, 0)",
                params![joined, text, freq, abbrev],
            )?;
            count += added;
        }

        tx.commit()?;
        Ok(count)
    }

    /// 精确查询：读音序列 → 候选，freq 降序 LIMIT limit
    pub fn lookup(&self, reading: &[String], limit: usize) -> Result<Vec<Candidate>> {
        if reading.is_empty() {
            return Ok(Vec::new());
        }
        let joined = reading.join("'");
        let mut stmt = self.conn.prepare(
            "SELECT text, pinyin, freq FROM phrase
             WHERE pinyin = ?1
             ORDER BY freq DESC, text ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![joined, limit as i64], |row| {
            Ok(Candidate {
                text: row.get(0)?,
                pinyin: row.get(1)?,
                freq: row.get::<_, i64>(2)? as u64,
                ai: false,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
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
        let mut stmt = self.conn.prepare(
            "SELECT text, pinyin, freq FROM phrase
             WHERE pinyin LIKE ?1
             ORDER BY freq DESC, text ASC
             LIMIT ?2",
        )?;
        let pattern = format!("{}%", joined);
        let rows = stmt.query_map(params![pattern, limit as i64], |row| {
            Ok(Candidate {
                text: row.get(0)?,
                pinyin: row.get(1)?,
                freq: row.get::<_, i64>(2)? as u64,
                ai: false,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 学习：用户选定 (读音, 词) → bump 用户词频 / 插入用户词
    pub fn learn(&mut self, reading: &[String], text: &str) -> Result<()> {
        if reading.is_empty() {
            return Ok(());
        }
        let joined = reading.join("'");
        let abbrev: String = reading.iter().filter_map(|s| s.chars().next()).collect();

        // bump freq if the (pinyin, text) already exists, otherwise insert a user row.
        let updated = self.conn.execute(
            "UPDATE phrase SET freq = freq + 1 WHERE pinyin = ?1 AND text = ?2",
            params![joined, text],
        )?;
        if updated == 0 {
            self.conn.execute(
                "INSERT INTO phrase(pinyin, text, freq, abbrev, user) VALUES (?1, ?2, 1, ?3, 1)",
                params![joined, text, abbrev],
            )?;
        }
        Ok(())
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
        let pattern = format!("{}%", initials);
        let mut stmt = self.conn.prepare(
            "SELECT text, pinyin, freq FROM phrase
             WHERE abbrev LIKE ?1
             ORDER BY freq DESC, text ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![pattern, limit as i64], |row| {
            Ok(Candidate {
                text: row.get(0)?,
                pinyin: row.get(1)?,
                freq: row.get::<_, i64>(2)? as u64,
                ai: false,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 用户词前 N：user=1 行按 freq DESC, text ASC（engine 个性化排序用）。
    pub fn top_user(&self, limit: usize) -> Result<Vec<Candidate>> {
        let mut stmt = self.conn.prepare(
            "SELECT text, pinyin, freq FROM phrase
             WHERE user = 1
             ORDER BY freq DESC, text ASC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(Candidate {
                text: row.get(0)?,
                pinyin: row.get(1)?,
                freq: row.get::<_, i64>(2)? as u64,
                ai: false,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
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

    fn seed(dict_yaml: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let yaml = std::env::temp_dir().join(format!(
            "kime_dict_lp_yaml_{}_{}.yaml",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = tmp_db("seed");
        fs::write(&yaml, dict_yaml).unwrap();
        let mut d = Dict::open(&db).unwrap();
        d.import(&yaml).unwrap();
        (db, yaml)
    }

    fn cleanup(db: &Path, yaml: &Path) {
        let _ = fs::remove_file(db);
        let _ = fs::remove_file(yaml);
    }

    #[test]
    fn lookup_prefix_empty_tail_equals_exact_lookup() {
        // 当 tail == ""，lookup_prefix 应该精确等于把 syllables 拼接后 lookup。
        let (db, yaml) = seed("...\n你好\tni hao\t5000\n我们\two men\t4000\n");
        let d = Dict::open(&db).unwrap();

        let exact = d.lookup(&["ni".into(), "hao".into()], 10).unwrap();
        let prefix = d
            .lookup_prefix(&["ni".into(), "hao".into()], "", 10)
            .unwrap();

        assert_eq!(exact.len(), prefix.len());
        assert_eq!(exact.len(), 1);
        assert_eq!(prefix[0].text, "你好");
        assert_eq!(prefix[0].pinyin, "ni'hao");
        assert_eq!(prefix[0].freq, 5000);

        cleanup(&db, &yaml);
    }

    #[test]
    fn lookup_prefix_partial_tail_returns_prefix_matches() {
        // "niha" — segments 会切成 ["ni","ha"] + tail=""; 这里我们手工模拟
        // 半截尾音节：syllables=["ni"] + tail="h" → 期望匹配 "ni'hao" 系列。
        let (db, yaml) = seed("...\n你好\tni hao\t5000\n泥猴\tni hou\t100\n");
        let d = Dict::open(&db).unwrap();

        let hits = d
            .lookup_prefix(&["ni".into()], "h", 10)
            .expect("prefix lookup");
        // "ni'hao..." 与 "ni'hou..." 都以 "ni'h" 开头 → 都命中
        let texts: Vec<&str> = hits.iter().map(|c| c.text.as_str()).collect();
        assert!(texts.contains(&"你好"));
        assert!(texts.contains(&"泥猴"));

        // 收紧 tail 到 "ha" → 只剩 ni'hao...
        let hits2 = d
            .lookup_prefix(&["ni".into()], "ha", 10)
            .expect("prefix ha");
        assert_eq!(hits2.len(), 1);
        assert_eq!(hits2[0].text, "你好");

        // tail 不匹配 → 0
        let hits3 = d.lookup_prefix(&["ni".into()], "z", 10).expect("prefix z");
        assert!(hits3.is_empty());

        cleanup(&db, &yaml);
    }

    #[test]
    fn lookup_prefix_respects_limit() {
        // 同前缀多条 → LIMIT 必须生效。
        let (db, yaml) = seed("...\nA\tha\t50\nB\tha\t40\nC\tha\t30\n");
        let d = Dict::open(&db).unwrap();

        let all = d.lookup_prefix(&["ha".into()], "", 10).unwrap();
        assert_eq!(all.len(), 3);

        let two = d.lookup_prefix(&["ha".into()], "", 2).unwrap();
        assert_eq!(two.len(), 2);
        // freq DESC tie-break by text ASC
        assert_eq!(two[0].text, "A");
        assert_eq!(two[1].text, "B");

        cleanup(&db, &yaml);
    }
}
