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

const MAGIC: &[u8; 4] = b"KIME";

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
        let file = File::open(bin_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        if mmap.len() < 8 || &mmap[..4] != MAGIC {
            anyhow::bail!("非法 dict.bin magic");
        }
        let fst_len = u32::from_le_bytes(mmap[4..8].try_into().unwrap()) as usize;
        let fst_bytes = &mmap[8..8 + fst_len];
        let values_start = 8 + fst_len;
        let values = mmap[values_start..].to_vec();

        let fst = Set::new(fst_bytes.to_vec())?;
        let keys: Vec<String> = fst.stream().into_strs().unwrap_or_default();
        let mut blocks: Vec<(String, usize)> = Vec::with_capacity(keys.len());
        let mut abbrev_index: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut pos = 0usize;
        for (key_idx, key) in keys.iter().enumerate() {
            let block_start = pos;
            if pos + 2 > values.len() {
                break;
            }
            let count = u16::from_le_bytes([values[pos], values[pos + 1]]) as usize;
            pos += 2;
            for _ in 0..count {
                if pos + 3 > values.len() {
                    break;
                }
                let text_len = (values[pos] as usize)
                    | ((values[pos + 1] as usize) << 8)
                    | ((values[pos + 2] as usize) << 16);
                pos += 3 + text_len + 4;
            }
            blocks.push((key.clone(), block_start));
            let abbrev: String = key.split('\'').filter_map(|s| s.chars().next()).collect();
            abbrev_index.entry(abbrev).or_default().push(key_idx);
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
        let next = (c as u32) + 1;
        new_chars[last_idx] = char::from_u32(next).unwrap_or(c);
    }
    Some(new_chars.into_iter().collect())
}

/// 解码 values 区一个块为候选词列表。
fn decode_block(data: &[u8], mut pos: usize, pinyin: &str) -> Vec<Candidate> {
    let mut out = Vec::new();
    if pos >= data.len() || data[pos] == 0 {
        return out;
    }
    let count = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
    pos += 2;
    for _ in 0..count {
        if pos + 3 > data.len() {
            break;
        }
        let text_len = (data[pos] as usize)
            | ((data[pos + 1] as usize) << 8)
            | ((data[pos + 2] as usize) << 16);
        pos += 3;
        if pos + text_len + 4 > data.len() {
            break;
        }
        let text = String::from_utf8_lossy(&data[pos..pos + text_len]).into_owned();
        pos += text_len;
        let freq =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as u64;
        pos += 4;
        out.push(Candidate {
            text,
            pinyin: pinyin.to_string(),
            freq,
            ai: false,
        });
    }
    out
}
