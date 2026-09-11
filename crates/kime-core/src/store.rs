//! FST 二进制词库只读存储（mmap）。
//!
//! 与 `builder.rs` 共享 dict.bin 格式契约。热路径（前缀查询）走 mmap 零拷贝，
//! 内核页缓存自动实现「热数据驻内存、冷数据留盘」的 lazy fetch。

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use fst::Set;
use fst::{IntoStreamer, Streamer};
use memmap2::Mmap;

use crate::dict::Candidate;

pub(crate) const MAGIC: &[u8; 4] = b"KIME";
/// 词库格式版本。v2：块候选计数 u16 → u32。v1 的 u16 在候选数 > 65535 时回绕
/// （rime-ice 的 `100` 组 980,961 条 → 63457），values 区扫描随之错位。
pub(crate) const FORMAT_VERSION: u32 = 2;
/// 头部：magic(4) + version(u32) + fst_len(u32)
pub(crate) const HEADER_LEN: usize = 12;
/// 块头：候选计数宽度
const COUNT_LEN: usize = 4;
/// 单条候选：[text_len: u24][text][freq: u32]
const TEXT_LEN_LEN: usize = 3;
const FREQ_LEN: usize = 4;

pub struct FstStore {
    _mmap: Mmap,
    fst: Set<Vec<u8>>,
    values: Vec<u8>,
    /// (key, block_offset_in_values) 按 FST 序
    blocks: Vec<(String, usize)>,
    abbrev_index: BTreeMap<String, Vec<usize>>, // abbrev -> indices into blocks
}

impl FstStore {
    pub fn open(bin_path: &Path) -> Result<Self> {
        let file = File::open(bin_path)
            .with_context(|| format!("打开 FST 词库失败: {}", bin_path.display()))?;
        let mmap = unsafe { Mmap::map(&file) }
            .with_context(|| format!("mmap FST 词库失败: {}", bin_path.display()))?;
        if mmap.len() < HEADER_LEN || &mmap[..4] != MAGIC {
            anyhow::bail!(
                "{} 不是合法的 dict.bin（magic 校验失败）",
                bin_path.display()
            );
        }
        let version = u32::from_le_bytes(mmap[4..8].try_into().unwrap());
        if version != FORMAT_VERSION {
            anyhow::bail!(
                "{} 是 dict.bin v{version}，本版本只支持 v{FORMAT_VERSION}，请重新 build-dict",
                bin_path.display()
            );
        }
        let fst_len = u32::from_le_bytes(mmap[8..HEADER_LEN].try_into().unwrap()) as usize;
        if HEADER_LEN + fst_len > mmap.len() {
            anyhow::bail!(
                "{} 头部声明 fst_len={} 超出文件大小 {}",
                bin_path.display(),
                fst_len,
                mmap.len()
            );
        }
        let fst_bytes = &mmap[HEADER_LEN..HEADER_LEN + fst_len];
        let values_start = HEADER_LEN + fst_len;
        let values = mmap[values_start..].to_vec();

        let fst = Set::new(fst_bytes.to_vec()).with_context(|| "FST 索引区解析失败")?;
        let keys: Vec<String> = fst.stream().into_strs().unwrap_or_default();
        let mut blocks: Vec<(String, usize)> = Vec::with_capacity(keys.len());
        let mut abbrev_index: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut pos = 0usize;
        for (key_idx, key) in keys.iter().enumerate() {
            let block_start = pos;
            pos = scan_block(&values, pos).with_context(|| {
                format!(
                    "{} values 区在第 {key_idx} 块（key={key}，偏移 {block_start}）错位，共 {} 字节",
                    bin_path.display(),
                    values.len()
                )
            })?;
            blocks.push((key.clone(), block_start));
            let abbrev: String = key.split('\'').filter_map(|s| s.chars().next()).collect();
            abbrev_index.entry(abbrev).or_default().push(key_idx);
        }
        // 完整性校验：块必须严丝合缝铺满 values 区，否则说明计数/长度错位（旧格式或被截断），
        // 交给 Dict::open 回退 SQLite 内存索引，绝不带着残缺 blocks 继续查。
        if pos != values.len() {
            anyhow::bail!(
                "{} values 区不完整：扫描消费 {pos}，实际 {} 字节 / {} 个 key",
                bin_path.display(),
                values.len(),
                keys.len()
            );
        }

        Ok(Self {
            _mmap: mmap,
            fst,
            values,
            blocks,
            abbrev_index,
        })
    }

    /// 前缀查询：完整音节 + 未完成尾音节。
    pub fn lookup_prefix(&self, syllables: &[String], tail: &str, limit: usize) -> Vec<Candidate> {
        let mut joined = syllables.join("'");
        if !tail.is_empty() {
            if !joined.is_empty() {
                joined.push('\'');
            }
            joined.push_str(tail);
        }
        if joined.is_empty() {
            return Vec::new();
        }

        let upper = increment_prefix(&joined);
        let stream_builder = if let Some(ref up) = upper {
            self.fst.range().ge(joined.as_str()).lt(up.as_str())
        } else {
            self.fst.range().ge(joined.as_str())
        };
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut stream = stream_builder.into_stream();
        while let Some(key) = stream.next() {
            let key_str = String::from_utf8_lossy(key).into_owned();
            let idx = self
                .blocks
                .binary_search_by(|(k, _)| k.as_str().cmp(&key_str));
            if let Ok(i) = idx {
                candidates.extend(decode_block(&self.values, self.blocks[i].1, &key_str));
            }
            if candidates.len() >= limit {
                break;
            }
        }
        candidates.sort_by(|a, b| b.freq.cmp(&a.freq).then_with(|| a.text.cmp(&b.text)));
        candidates.truncate(limit);
        candidates
    }

    /// 声母缩写查询。
    pub fn lookup_abbrev(&self, initials: &str, limit: usize) -> Vec<Candidate> {
        if initials.is_empty() || !initials.bytes().all(|b| b.is_ascii_lowercase()) {
            return Vec::new();
        }
        let mut candidates: Vec<Candidate> = Vec::new();
        for (abbrev, indices) in self.abbrev_index.range(initials.to_string()..) {
            if !abbrev.starts_with(initials) {
                break;
            }
            for &idx in indices {
                let (key, offset) = &self.blocks[idx];
                candidates.extend(decode_block(&self.values, *offset, key));
                if candidates.len() >= limit {
                    break;
                }
            }
            if candidates.len() >= limit {
                break;
            }
        }
        candidates.truncate(limit);
        candidates
    }
}

/// 计算前缀的排他上界（末字符 +1）；'z' 结尾返回 None。
fn increment_prefix(prefix: &str) -> Option<String> {
    let mut chars: Vec<char> = prefix.chars().collect();
    let last_idx = chars.len().checked_sub(1)?;
    let c = chars[last_idx];
    if c == 'z' {
        return None;
    }
    chars[last_idx] = if c == '\'' {
        'a'
    } else {
        char::from_u32(c as u32 + 1).unwrap_or(c)
    };
    Some(chars.into_iter().collect())
}

/// 读块头候选计数（u32 LE）；越界返回 None。
fn block_count(data: &[u8], start: usize) -> Option<usize> {
    let b = data.get(start..start + COUNT_LEN)?;
    Some(u32::from_le_bytes(b.try_into().unwrap()) as usize)
}

/// 读单条候选的 text_len（u24 LE）；越界返回 None。
fn text_len_at(data: &[u8], pos: usize) -> Option<usize> {
    let b = data.get(pos..pos + TEXT_LEN_LEN)?;
    Some(b[0] as usize | (b[1] as usize) << 8 | (b[2] as usize) << 16)
}

/// 跳过 values 区的一个块，返回下一块偏移；畸形或越界返回 None（open 转成 Err）。
fn scan_block(data: &[u8], start: usize) -> Option<usize> {
    let count = block_count(data, start)?;
    let mut pos = start + COUNT_LEN;
    for _ in 0..count {
        pos += TEXT_LEN_LEN + text_len_at(data, pos)? + FREQ_LEN;
        if pos > data.len() {
            return None;
        }
    }
    Some(pos)
}

/// 解码 values 区一个块为候选词列表。
///
/// 旧代码用「块首字节 == 0」判空块：计数换成 u32 LE 之后 256 条的块首字节同样是 0，
/// 该捷径会静默吞掉整块，故改为按 count 走。
fn decode_block(data: &[u8], mut pos: usize, pinyin: &str) -> Vec<Candidate> {
    let mut out = Vec::new();
    let Some(count) = block_count(data, pos) else {
        return out;
    };
    pos += COUNT_LEN;
    for _ in 0..count {
        let Some(text_len) = text_len_at(data, pos) else {
            break;
        };
        pos += TEXT_LEN_LEN;
        if pos + text_len + FREQ_LEN > data.len() {
            break;
        }
        let text = String::from_utf8_lossy(&data[pos..pos + text_len]).into_owned();
        pos += text_len;
        let freq = u32::from_le_bytes(data[pos..pos + FREQ_LEN].try_into().unwrap()) as u64;
        pos += FREQ_LEN;
        out.push(Candidate {
            text,
            pinyin: pinyin.to_string(),
            freq,
            ai: false,
        });
    }
    out
}
