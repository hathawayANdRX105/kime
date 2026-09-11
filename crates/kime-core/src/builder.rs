//! 构建 FST 二进制词库（编译期产物）。
//!
//! dict.bin 布局（builder 写 / store 读，必须严格一致）：
//!
//! ```text
//! [magic: 4 b"KIME"]
//! [version: u32 LE]            // 当前 = 2（v1 块计数为 u16，已废弃）
//! [fst_len: u32 LE]            // FST 序列化后的字节数
//! [fst_bytes: <fst_len>]       // fst::Set，key = pinyin（`'` 连接），字典序
//! [values: per-key blocks]      // 与 FST key 同序
//!   对每个 key（按 FST 序）：
//!     [count: u32 LE] × 该拼音的候选数
//!     count 次：[text_len: u24 LE (3B)][text: utf8][freq: u32 LE]
//! ```

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result};
use fst::SetBuilder;
use rusqlite::Connection;

use crate::dict::Candidate;

/// 从 SQLite 词库构建 FST 二进制文件。
///
/// 排序键 (pinyin ASC, freq DESC, text ASC) 与 Dict 查询契约一致。
pub fn build(dict_sqlite: &Path, out_bin: &Path) -> Result<u64> {
    let conn = Connection::open(dict_sqlite)
        .with_context(|| format!("打开 SQLite 失败: {}", dict_sqlite.display()))?;
    let mut stmt = conn.prepare(
        "SELECT pinyin, text, freq FROM phrase ORDER BY pinyin ASC, freq DESC, text ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)? as u64,
        ))
    })?;

    // 按 pinyin 分组，保持 (freq DESC, text ASC) 序
    let mut groups: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
    for row in rows {
        let (pinyin, text, freq) = row?;
        groups.entry(pinyin).or_default().push(Candidate {
            text,
            pinyin: String::new(),
            freq,
            ai: false,
        });
    }

    let pinyins: Vec<String> = groups.keys().cloned().collect(); // BTreeMap 已排序
    let mut set_builder = SetBuilder::memory();
    for p in &pinyins {
        set_builder.insert(p)?;
    }
    let fst_bytes = set_builder.into_inner()?;
    let fst_len = fst_bytes.len() as u32;

    // values 区：按 pinyin 序写块
    let mut values: Vec<u8> = Vec::new();
    let mut total_candidates = 0u64;
    for pinyin in &pinyins {
        let cands = groups.get(pinyin).unwrap();
        // 计数必须是 u32：rime-ice 的 pinyin='100' 组有 98 万条，u16 会回绕成 63457。
        let Ok(count) = u32::try_from(cands.len()) else {
            anyhow::bail!("单拼音候选数超出 u32: {pinyin} = {}", cands.len());
        };
        values.extend_from_slice(&count.to_le_bytes());
        total_candidates += cands.len() as u64;
        for cand in cands {
            let bytes = cand.text.as_bytes();
            let len = bytes.len();
            if len > 0xFFFFFF {
                anyhow::bail!("候选文本过长: {} > 16MB", len);
            }
            // text_len 存 3 字节 u24 LE
            values.push((len & 0xFF) as u8);
            values.push(((len >> 8) & 0xFF) as u8);
            values.push(((len >> 16) & 0xFF) as u8);
            values.extend_from_slice(bytes);
            values.extend_from_slice(&(cand.freq as u32).to_le_bytes());
        }
    }

    let mut file = BufWriter::new(File::create(out_bin)?);
    file.write_all(crate::store::MAGIC)?;
    file.write_all(&crate::store::FORMAT_VERSION.to_le_bytes())?;
    file.write_all(&fst_len.to_le_bytes())?;
    file.write_all(&fst_bytes)?;
    file.write_all(&values)?;
    file.flush()?;

    Ok(total_candidates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn seed_sqlite(path: &PathBuf) {
        let conn = Connection::open(path).unwrap();
        conn.execute(
            "CREATE TABLE phrase (pinyin TEXT NOT NULL, text TEXT NOT NULL, freq INTEGER NOT NULL DEFAULT 0, abbrev TEXT NOT NULL, user INTEGER NOT NULL DEFAULT 0, UNIQUE(pinyin,text))",
            [],
        ).unwrap();
        let mut stmt = conn
            .prepare("INSERT INTO phrase(pinyin,text,freq,abbrev,user) VALUES (?1,?2,?3,?4,0)")
            .unwrap();
        for (p, t, f) in [
            ("ni'hao", "你好", 5000i64),
            ("ni'hao", "拟好", 100),
            ("shen'me", "什么", 8000),
            ("shen'me", "审美", 500),
        ] {
            stmt.execute(rusqlite::params![p, t, f, "sm"]).unwrap();
        }
    }

    #[test]
    fn build_then_open_roundtrip() {
        let db = NamedTempFile::new().unwrap();
        seed_sqlite(&db.path().to_path_buf());
        let bin = NamedTempFile::new().unwrap();
        let n = build(db.path(), bin.path()).unwrap();
        assert_eq!(n, 4, "应写入 4 条候选");

        let store = crate::store::FstStore::open(bin.path()).unwrap();
        let hits = store.lookup_prefix(&["ni".into()], "hao", 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].text, "你好"); // freq 5000 > 100
        assert_eq!(hits[1].text, "拟好");

        let sm = store.lookup_prefix(&["shen".into()], "me", 10);
        assert_eq!(sm.len(), 2);
        assert_eq!(sm[0].text, "什么");

        // abbrev
        let abbrev = store.lookup_abbrev("sm", 10);
        assert_eq!(abbrev.len(), 2);

        // 不存在
        let none = store.lookup_prefix(&["zzz".into()], "", 10);
        assert!(none.is_empty());
    }
}
