//! 构建 FST 二进制词库（编译期产物）。
//!
//! dict.bin v3 布局与 `store.rs` 顶部文档一致（builder 写 / store 读，必须严格一致）：
//!
//! ```text
//! [magic: 4 b"KIME"]
//! [version: u32 LE]            // 当前 = 3
//! [fst_len: u32] [n_keys: u32]
//! [values_len: u64] [keys_len: u64] [topk_fst_len: u64] [topk_blob_len: u64] [abbrev_len: u64]
//! [fst: fst::Map<pinyin, key_idx>]
//! [block_offsets: n_keys × u32]      // 第 i 块在 values 区内的起始偏移
//! [values: 与 v2 相同的逐 key 块编码] // 块内 (freq DESC, text ASC)
//! [key_offsets: (n_keys+1) × u32] [key_blob]
//! [topk_fst: fst::Map<前缀, 桶偏移>] [topk_blob]  // 首段前缀全局 top-64，桶=[count u16]{key_idx,freq,tlen,text}
//! [abbrev_offsets: (n_keys+1) × u32] [abbrev_records] // [name u8][abbrev][key_idx u32]，全局排序
//! ```

use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result};
use fst::MapBuilder;
use rusqlite::Connection;

use crate::dict::Candidate;

/// 与 `store::TOPK` 同步：热表每桶条数。
const TOPK: usize = 64;

/// 热表桶内的全序：freq DESC, text ASC, key_idx ASC（末位消除跨块同分歧义）。
fn bucket_cmp(a: &BucketEntry, b: &BucketEntry) -> Ordering {
    b.freq
        .cmp(&a.freq)
        .then_with(|| a.text.cmp(&b.text))
        .then_with(|| a.key_idx.cmp(&b.key_idx))
}

/// 桶内 top-K 有界堆：堆顶恒为「当前最差者」，满员后来者更优才置换。
struct BucketEntry {
    key_idx: u32,
    freq: u64,
    text: String,
}
impl PartialEq for BucketEntry {
    fn eq(&self, o: &Self) -> bool {
        self.key_idx == o.key_idx && self.freq == o.freq && self.text == o.text
    }
}
impl Eq for BucketEntry {}
impl PartialOrd for BucketEntry {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for BucketEntry {
    // 堆顶=最差者：self 在输出序(bucket_cmp)中排在 o 之后 ⟺ self 更差 ⟺ Greater
    fn cmp(&self, o: &Self) -> Ordering {
        bucket_cmp(self, o)
    }
}

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
    let n_keys = pinyins.len();
    let n_keys32 = u32::try_from(n_keys).context("key 数超出 u32")?;

    let mut map_builder = MapBuilder::memory();
    for (i, p) in pinyins.iter().enumerate() {
        map_builder.insert(p, i as u64)?;
    }
    let fst_bytes = map_builder.into_inner()?;
    let fst_len = u32::try_from(fst_bytes.len()).context("FST 区超出 u32")?;

    // values 区：按 pinyin 序写块；block_offsets 与 FST value 同序一一对应。
    let mut values: Vec<u8> = Vec::new();
    let mut block_offsets: Vec<u32> = Vec::with_capacity(n_keys);
    let mut total_candidates = 0u64;
    for pinyin in &pinyins {
        let cands = groups.get(pinyin).unwrap();
        block_offsets.push(u32::try_from(values.len()).context("values 区超出 u32 偏移范围")?);
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

    // key 名表：key_idx → utf8 字节，abbrev / topk 路径回填 Candidate.pinyin 用。
    let mut key_offsets: Vec<u32> = Vec::with_capacity(n_keys + 1);
    let mut key_blob: Vec<u8> = Vec::new();
    for p in &pinyins {
        key_offsets.push(u32::try_from(key_blob.len()).context("key_blob 超出 u32")?);
        key_blob.extend_from_slice(p.as_bytes());
    }
    key_offsets.push(u32::try_from(key_blob.len()).context("key_blob 超出 u32")?);

    // 热表：首段（`'` 前）的每个全小写字母前缀一个桶，桶内全局 top-K。
    // 构建期一次全扫描换运行期「首音节/首字母宽前缀」查询 O(1)——v2 的每键 30–98ms 就慢在这种区间。
    let mut buckets: BTreeMap<&str, BinaryHeap<BucketEntry>> = BTreeMap::new();
    for (key_idx, pinyin) in pinyins.iter().enumerate() {
        let key_idx = key_idx as u32;
        let seg_end = pinyin.find('\'').unwrap_or(pinyin.len());
        for plen in 1..=seg_end {
            if !pinyin.as_bytes()[plen - 1].is_ascii_lowercase() {
                break; // 前缀必须整体是 a–z，与查询端条件一致
            }
            let heap = buckets.entry(&pinyin[..plen]).or_default();
            for cand in groups.get(pinyin.as_str()).unwrap() {
                let entry = BucketEntry {
                    key_idx,
                    freq: cand.freq,
                    text: cand.text.clone(),
                };
                if heap.len() < TOPK {
                    heap.push(entry);
                } else if bucket_cmp(&entry, heap.peek().unwrap()) == Ordering::Less {
                    heap.pop();
                    heap.push(entry);
                }
            }
        }
    }
    let mut topk_builder = MapBuilder::memory();
    let mut topk_blob: Vec<u8> = Vec::new();
    for (prefix, heap) in buckets {
        topk_builder.insert(prefix, topk_blob.len() as u64)?;
        let mut entries: Vec<BucketEntry> = heap.into_vec();
        entries.sort_by(bucket_cmp);
        let count = u16::try_from(entries.len()).expect("TOPK 不超 u16");
        topk_blob.extend_from_slice(&count.to_le_bytes());
        for e in &entries {
            let tlen = u16::try_from(e.text.len()).context("热表候选文本超出 u16")?;
            topk_blob.extend_from_slice(&e.key_idx.to_le_bytes());
            topk_blob.extend_from_slice(&(e.freq as u32).to_le_bytes());
            topk_blob.extend_from_slice(&tlen.to_le_bytes());
            topk_blob.extend_from_slice(e.text.as_bytes());
        }
    }
    let topk_fst = topk_builder.into_inner()?;

    // abbrev 表：每 key 一项 (abbrev, key_idx)，全局排序后平铺；store 侧二分 + 顺序扫。
    let mut abbrev_entries: Vec<(String, u32)> = Vec::with_capacity(n_keys);
    for (i, p) in pinyins.iter().enumerate() {
        let abbrev: String = p.split('\'').filter_map(|s| s.chars().next()).collect();
        abbrev_entries.push((abbrev, i as u32));
    }
    abbrev_entries.sort();
    let mut abbrev_offsets: Vec<u32> = Vec::with_capacity(n_keys + 1);
    let mut abbrev_records: Vec<u8> = Vec::new();
    for (abbrev, key_idx) in &abbrev_entries {
        let nlen = u8::try_from(abbrev.len()).context("abbrev 长度超出 u8")?;
        abbrev_offsets.push(u32::try_from(abbrev_records.len()).context("abbrev 区超出 u32")?);
        abbrev_records.push(nlen);
        abbrev_records.extend_from_slice(abbrev.as_bytes());
        abbrev_records.extend_from_slice(&key_idx.to_le_bytes());
    }
    abbrev_offsets.push(u32::try_from(abbrev_records.len()).context("abbrev 区超出 u32")?);

    // 落盘（区序必须与 store::open 的解析序一致）
    let mut file = BufWriter::new(File::create(out_bin)?);
    file.write_all(crate::store::MAGIC)?;
    file.write_all(&crate::store::FORMAT_VERSION.to_le_bytes())?;
    file.write_all(&fst_len.to_le_bytes())?;
    file.write_all(&n_keys32.to_le_bytes())?;
    for len in [
        values.len() as u64,
        key_blob.len() as u64,
        topk_fst.len() as u64,
        topk_blob.len() as u64,
        abbrev_records.len() as u64,
    ] {
        file.write_all(&len.to_le_bytes())?;
    }
    file.write_all(&fst_bytes)?;
    for v in &block_offsets {
        file.write_all(&v.to_le_bytes())?;
    }
    file.write_all(&values)?;
    for v in &key_offsets {
        file.write_all(&v.to_le_bytes())?;
    }
    file.write_all(&key_blob)?;
    file.write_all(&topk_fst)?;
    file.write_all(&topk_blob)?;
    for v in &abbrev_offsets {
        file.write_all(&v.to_le_bytes())?;
    }
    file.write_all(&abbrev_records)?;
    file.flush()?;

    Ok(total_candidates)
}

#[cfg(test)]
mod tests {
    use super::*;
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
