//! FST 二进制词库只读存储（mmap）。
//!
//! 与 `builder.rs` 共享 dict.bin 格式契约。热路径（前缀查询）走 mmap 零拷贝，
//! 内核页缓存自动实现「热数据驻内存、冷数据留盘」的 lazy fetch。
//!
//! ## dict.bin v3 布局（多字节整数一律 LE；builder 写 / store 读，必须严格一致）
//!
//! ```text
//! [0]  magic: 4 b"KIME"
//! [4]  version: u32 = 3
//! [8]  fst_len: u32            // FST Map 区字节数
//! [12] n_keys: u32             // key 数 = 块数 = abbrev 表项数
//! [16] values_len: u64
//! [24] keys_len: u64           // key 名字 blob 字节数
//! [32] topk_fst_len: u64       // 热表 FST Map 区字节数
//! [40] topk_blob_len: u64      // 热表记录 blob 字节数
//! [48] abbrev_len: u64         // abbrev records 区字节数（其偏移表定长 (n_keys+1)×4）
//! [56] fst: <fst_len>          // fst::Map，key = pinyin（`'` 连接），value = key_idx（FST 序下标）
//!      block_offsets: n_keys×u32        // 第 i 块在 values 区内的起始偏移；[0]=0，严格递增，均 < values_len
//!      values: <values_len>             // 与 v2 相同的逐 key 块编码：
//!                                       //   [count: u32] + count × [text_len: u24][text][freq: u32]
//!                                       //   块内序 = (freq DESC, text ASC)，热路径依此只解每块前 limit 条
//!      key_offsets: (n_keys+1)×u32      // key_blob 中第 i 个 key 的起止；非降，[0]=0，[n]=keys_len
//!      key_blob: <keys_len>             // 每个 key 的 utf8 字节（供 abbrev/topk 路径回填 Candidate.pinyin）
//!      topk_fst: <topk_fst_len>         // fst::Map，key = 词库首音节串的每个全小写字母前缀（不含 `'`），
//!                                       //   value = 该前缀在 topk_blob 内的记录偏移
//!      topk_blob: <topk_blob_len>       // 每桶：[count: u16] + count × [key_idx: u32][freq: u32][tlen: u16][text]
//!                                       //   桶内序 = (freq DESC, text ASC, key_idx ASC)，全局 top-K（K=64），
//!                                       //   构建期完整算好：首键 1–2 字母/整音节的宽前缀查询 O(1) 取回，不再遍历 FST
//!      abbrev_offsets: (n_keys+1)×u32   // records 中第 i 项的起始；[0]=0，严格递增，[n]=abbrev_len
//!      abbrev_records: <abbrev_len>     // 每 key 一项 [name_len: u8][abbrev][key_idx: u32]，
//!                                       //   abbrev = 各音节首字符，全局按 (abbrev ASC, key_idx ASC) 排序
//! ```
//!
//! v2 → v3：块偏移表与 abbrev 索引移到构建期落盘，`open` 不再物化全部 key、
//! 不再步进候选（v2 的 5s 开库由此而来）；values 不再整区 `to_vec()` 堆拷贝。
//! 开库校验为 O(n_keys)（偏移单调、各区恰好铺满文件、末块 scan 到 values 末尾），
//! 畸形/截断/旧版本一律 Err，交给 `Dict::open` 回退 SQLite 内存索引。

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use fst::{IntoStreamer, Map, Streamer};
use memmap2::Mmap;

use crate::dict::Candidate;

pub(crate) const MAGIC: &[u8; 4] = b"KIME";
/// 词库格式版本。v2：块候选计数 u16 → u32（v1 在候选数 > 65535 时回绕）。
/// v3：块偏移表 / key 名表 / abbrev 表 / 首段前缀 top-K 热表落盘；FST 由 Set 改为 Map。
pub(crate) const FORMAT_VERSION: u32 = 3;
/// 头部：magic(4) + version + fst_len + n_keys (各 u32) + 5 个 u64 区长度
pub(crate) const HEADER_LEN: usize = 56;
/// 块头：候选计数宽度
const COUNT_LEN: usize = 4;
/// 单条候选：[text_len: u24][text][freq: u32]
const TEXT_LEN_LEN: usize = 3;
const FREQ_LEN: usize = 4;
/// 热表每桶条数。查询 limit ≤ TOPK 时宽前缀直接查表；更大 limit 退化为全区间归并。
const TOPK: usize = 64;

pub struct FstStore {
    _mmap: Mmap,
    /// FST Map：pinyin → key_idx（字节在 open 时一次性拷出 ~6MB，避免自引用）
    fst: Map<Vec<u8>>,
    n_keys: usize,
    offs_start: usize,
    values_start: usize,
    values_len: usize,
    koffs_start: usize,
    kblob_start: usize,
    topk: Map<Vec<u8>>,
    topk_blob_start: usize,
    atbl_start: usize,
    arec_start: usize,
    abbrev_len: usize,
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
        let u32_at =
            |range: std::ops::Range<usize>| u32::from_le_bytes(mmap[range].try_into().unwrap());
        let u64_at =
            |range: std::ops::Range<usize>| u64::from_le_bytes(mmap[range].try_into().unwrap());
        let fst_len = u32_at(8..12) as usize;
        let n_keys = u32_at(12..16) as usize;
        let values_len = u64_at(16..24) as usize;
        let keys_len = u64_at(24..32) as usize;
        let topk_fst_len = u64_at(32..40) as usize;
        let topk_blob_len = u64_at(40..48) as usize;
        let abbrev_len = u64_at(48..56) as usize;

        // 各区必须严丝合缝铺满文件：截断、多余尾巴、v2 残留都会在这里被拒。
        let mut pos = HEADER_LEN;
        let mut region = |len: usize| -> Result<usize> {
            let start = pos;
            pos = pos.checked_add(len).context("dict.bin 区长度总和溢出")?;
            if pos > mmap.len() {
                anyhow::bail!(
                    "{} 区越界：声明至 {pos}，文件仅 {} 字节",
                    bin_path.display(),
                    mmap.len()
                );
            }
            Ok(start)
        };
        let offs_tbl = n_keys.checked_mul(4).context("n_keys 溢出")?;
        let idx_tbl = n_keys
            .checked_add(1)
            .and_then(|v| v.checked_mul(4))
            .context("n_keys 溢出")?;
        let fst_start = region(fst_len)?;
        let offs_start = region(offs_tbl)?;
        let values_start = region(values_len)?;
        let koffs_start = region(idx_tbl)?;
        let kblob_start = region(keys_len)?;
        let topk_fst_start = region(topk_fst_len)?;
        let topk_blob_start = region(topk_blob_len)?;
        let atbl_start = region(idx_tbl)?;
        let arec_start = region(abbrev_len)?;
        if arec_start + abbrev_len != mmap.len() {
            anyhow::bail!(
                "{} 各区长度与文件大小不符：声明 {total}，实际 {}",
                bin_path.display(),
                mmap.len(),
                total = arec_start + abbrev_len
            );
        }

        let fst = Map::new(mmap[fst_start..fst_start + fst_len].to_vec())
            .with_context(|| "FST 索引区解析失败")?;
        let topk = Map::new(mmap[topk_fst_start..topk_fst_start + topk_fst_len].to_vec())
            .with_context(|| "top-K 热表 FST 解析失败")?;

        // —— O(n_keys) 廉价校验：三张偏移表一趟扫完（不再步进候选、不再逐次 get+closure）——
        let (mut prev_key, mut prev_ab) = (0u32, 0u32);
        let keys_len32 = keys_len as u32;
        let abbrev_len32 = abbrev_len as u32;
        let offs_t = mmap
            .get(offs_start..offs_start + offs_tbl)
            .context("block_offsets 越界")?;
        let koffs_t = mmap
            .get(koffs_start..koffs_start + idx_tbl)
            .context("key_offsets 越界")?;
        let aboffs_t = mmap
            .get(atbl_start..atbl_start + idx_tbl)
            .context("abbrev_offsets 越界")?;
        let bo = offs_t.as_chunks::<4>().0;
        let ko = koffs_t.as_chunks::<4>().0;
        let ao = aboffs_t.as_chunks::<4>().0;
        let mut last_block = 0usize; // 末块在 values 区内的起始偏移
        for i in 0..n_keys {
            // block_offsets：[0]=0，严格递增，每项 < values_len
            let v = u32::from_le_bytes(bo[i]) as usize;
            if (i == 0 && v != 0) || (i > 0 && (v <= last_block || v >= values_len)) {
                anyhow::bail!(
                    "{} block_offsets 非法于第 {i} 块（v={v}）",
                    bin_path.display()
                );
            }
            last_block = v;
            // key_offsets：[0]=0，非降，每项 ≤ keys_len；abbrev_offsets：严格递增，每项 ≤ abbrev_len
            let kv = u32::from_le_bytes(ko[i]);
            let av = u32::from_le_bytes(ao[i]);
            if (i == 0 && (kv != 0 || av != 0))
                || kv > keys_len32
                || av > abbrev_len32
                || (i > 0 && (kv < prev_key || av <= prev_ab))
            {
                anyhow::bail!("{} 索引表第 {i} 项非法", bin_path.display());
            }
            prev_key = kv;
            prev_ab = av;
        }
        // 表尾项（n_keys 下标）单独核对
        if n_keys > 0 {
            let kv = u32::from_le_bytes(ko[n_keys]);
            let av = u32::from_le_bytes(ao[n_keys]);
            if kv < prev_key || kv > keys_len32 || av <= prev_ab || av > abbrev_len32 {
                anyhow::bail!("{} 索引表尾项非法", bin_path.display());
            }
            prev_key = kv;
            prev_ab = av;
        }
        if n_keys > 0 {
            if prev_key != keys_len32 {
                anyhow::bail!("{} key_offsets[n] 必须等于 keys_len", bin_path.display());
            }
            if prev_ab != abbrev_len32 {
                anyhow::bail!(
                    "{} abbrev_offsets[n] 必须等于 abbrev_len",
                    bin_path.display()
                );
            }
            // 末块 scan 到 values 末尾：完整性闭环，仅 O(最后一块候选数)。
            let want = values_len - last_block;
            let last = scan_block(
                &mmap[values_start + last_block..values_start + values_len],
                0,
            )
            .with_context(|| {
                format!(
                    "{} 末块在 values 区偏移 {last_block} 处错位",
                    bin_path.display()
                )
            })?;
            if last != want {
                anyhow::bail!(
                    "{} values 区不完整：末块消费 {last}，应为 {want}",
                    bin_path.display()
                );
            }
        }

        Ok(Self {
            _mmap: mmap,
            fst,
            n_keys,
            offs_start,
            values_start,
            values_len,
            koffs_start,
            kblob_start,
            topk,
            topk_blob_start,
            atbl_start,
            arec_start,
            abbrev_len,
        })
    }

    #[inline]
    fn values(&self) -> &[u8] {
        &self._mmap[self.values_start..self.values_start + self.values_len]
    }

    /// key_idx → 其块在 values 区的切片（自块首到 values 区尾，解码按 limit/边界自止）。
    #[inline]
    fn block(&self, key_idx: u64) -> Option<&[u8]> {
        let i = usize::try_from(key_idx).ok()?;
        let base = self.offs_start.checked_add(i.checked_mul(4)?)?;
        let b = self._mmap.get(base..base.checked_add(4)?)?;
        let off = u32::from_le_bytes(b.try_into().unwrap()) as usize;
        self.values().get(off..)
    }

    #[inline]
    fn key_name(&self, key_idx: u64) -> Option<&[u8]> {
        let i = usize::try_from(key_idx).ok()?;
        let s = {
            let base = self.koffs_start + i * 4;
            let b = self._mmap.get(base..base + 4)?;
            u32::from_le_bytes(b.try_into().unwrap())
        };
        let e = {
            let base = self.koffs_start + (i + 1) * 4;
            let b = self._mmap.get(base..base + 4)?;
            u32::from_le_bytes(b.try_into().unwrap())
        };
        self._mmap
            .get(self.kblob_start + s as usize..self.kblob_start + e as usize)
    }

    /// 热表取回：前缀桶的预计算全局 top-K，零遍历。
    fn fetch_topk(&self, off: usize, limit: usize) -> Vec<Candidate> {
        let mut out = Vec::new();
        let Some(blob) = self._mmap.get(self.topk_blob_start + off..) else {
            return out;
        };
        let Some(cnt_b) = blob.get(0..2) else {
            return out;
        };
        let count = u16::from_le_bytes(cnt_b.try_into().unwrap()) as usize;
        let mut pos = 2usize;
        for _ in 0..count {
            if out.len() >= limit {
                break;
            }
            let rec = match blob.get(pos..pos + 10) {
                Some(r) => r,
                None => break,
            };
            let key_idx = u32::from_le_bytes(rec[0..4].try_into().unwrap()) as u64;
            let freq = u32::from_le_bytes(rec[4..8].try_into().unwrap()) as u64;
            let tlen = u16::from_le_bytes(rec[8..10].try_into().unwrap()) as usize;
            pos += 10;
            let text = match blob.get(pos..pos + tlen) {
                Some(t) => t,
                None => break,
            };
            pos += tlen;
            let pinyin = match self.key_name(key_idx) {
                Some(k) => String::from_utf8_lossy(k).into_owned(),
                None => String::new(),
            };
            out.push(Candidate {
                text: String::from_utf8_lossy(text).into_owned(),
                pinyin,
                freq,
                ai: false,
            });
        }
        out
    }

    /// 把 key_idx 所指块的前 `limit` 条（块内已按 freq DESC, text ASC 排好）归并进 top-k 堆。
    /// 堆满 limit 后先做块头剪枝：块内最优都不敌当前第 limit 名 → 整块跳过，不产生任何分配。
    fn collect_block(
        &self,
        key_idx: u64,
        key_hint: Option<&[u8]>,
        top: &mut BinaryHeap<Worst>,
        limit: usize,
    ) {
        let Some(data) = self.block(key_idx) else {
            return;
        };
        if top.len() == limit {
            let worst = &top.peek().unwrap().0;
            match block_head(data) {
                Some((freq, text)) => {
                    if !(freq > worst.freq || (freq == worst.freq && text < worst.text.as_bytes()))
                    {
                        return;
                    }
                }
                None => return,
            }
        }
        let pinyin: String = match key_hint {
            Some(k) => String::from_utf8_lossy(k).into_owned(),
            None => match self.key_name(key_idx) {
                Some(k) => String::from_utf8_lossy(k).into_owned(),
                None => return,
            },
        };
        for c in decode_block(data, &pinyin, limit) {
            if top.len() == limit {
                let worst = &top.peek().unwrap().0;
                if !(c.freq > worst.freq || (c.freq == worst.freq && c.text < worst.text)) {
                    break; // 块内降序，此后更无胜算
                }
                top.pop();
            }
            top.push(Worst(c));
        }
    }

    /// 精确查询：`syllables` 必须与 key 完全相等。FST 是 `Map<pinyin, key_idx>`，一次 get 即可。
    ///
    /// 与 `lookup_prefix(syllables, "", limit)` 不同——后者层二仍收 `ni'hao'…` 这类
    /// 跨边界补全 key（见 `lookup_prefix_layers`）。Viterbi 按跨度 (i,j) 查词，必须只拿
    /// 该跨度的词，否则 2 音节跨度返回 4 音节的词，格子里的词互相重叠，整句结果变成乱码。
    pub fn lookup_exact(&self, syllables: &[String], limit: usize) -> Vec<Candidate> {
        if limit == 0 || syllables.is_empty() {
            return Vec::new();
        }
        let joined = syllables.join("'");
        let Some(key_idx) = self.fst.get(joined.as_str()) else {
            return Vec::new();
        };
        let Some(data) = self.block(key_idx) else {
            return Vec::new();
        };
        // 块内已按 (freq DESC, text ASC) 写入，解到 limit 即止。
        decode_block(data, &joined, limit)
    }

    /// 前缀查询（合并形态）：层一（精确命中）在前、层二（补全）在后，各自内部
    /// (freq DESC, text ASC)。语义见 [`Self::lookup_prefix_layers`]。
    pub fn lookup_prefix(&self, syllables: &[String], tail: &str, limit: usize) -> Vec<Candidate> {
        let (exact, comps) = self.lookup_prefix_layers(syllables, tail, limit);
        let mut out = exact;
        out.extend(comps);
        out.truncate(limit);
        out
    }

    /// 两层前缀查询：`(层一 = key 恰为 joined 的块, 层二 = 合法补全)`。
    ///
    /// 层二的区间按 tail 是否打完分两种（这就是「打 min 出明/名/命」的修复点）：
    /// - **tail 非空**（最后一个音节还在打）：开区间 `[joined, increment_prefix(joined))`
    ///   去掉精确 key 自身——`mi` 仍能补全出 `min`、`ming`、`mi'xxx`（用户可能正打任何
    ///   以 mi 开头的音节，这里砍不得）。
    /// - **tail 为空**（音节边界已成）：只允许**跨音节边界**的延展，即 key 以
    ///   `joined + "'"` 开头。`ming` 不是 `min` 的补全——它要求用户在不换边界的前提下
    ///   往同一音节里再敲字母；`min'xxx` 才是合法补全。上界用
    ///   `increment_prefix("min'") = "mina"`：`'`(0x27) 是 a–z 表之前的字节，
    ///   `"min'…"` 全部 < `"mina"`，且任何以 `mina` 开头的 key 必不以 `min'` 开头，紧致。
    ///
    /// **tail 为空时不走 topk 热表**：热表桶按「首音节的裸字母前缀」在构建期预计算，
    /// 桶内没有 `'` 边界概念，`min` 桶天然装着 `ming` 的条目——正是本修复要排除的集合。
    /// tail 非空时热表仍可用：开区间本就是桶的语义，只需滤掉精确 key 的条目
    /// （滤后可能少 1–2 条桶深之外的候选，与原实现的 TOPK 截断同级，不更差）。
    pub fn lookup_prefix_layers(
        &self,
        syllables: &[String],
        tail: &str,
        limit: usize,
    ) -> (Vec<Candidate>, Vec<Candidate>) {
        if limit == 0 {
            return (Vec::new(), Vec::new());
        }
        let mut joined = syllables.join("'");
        if !tail.is_empty() {
            if !joined.is_empty() {
                joined.push('\'');
            }
            joined.push_str(tail);
        }
        if joined.is_empty() {
            return (Vec::new(), Vec::new());
        }

        // 层一：一次 fst.get + 单块解码（块内已按 freq DESC 排好）。不借热表——
        // 桶是全局 top-64，精确块里的低频字（民→闵）可能被挤出，单查该块才完整。
        let exact = match self
            .fst
            .get(joined.as_str())
            .and_then(|idx| self.block(idx))
        {
            Some(data) => decode_block(data, &joined, limit),
            None => Vec::new(),
        };

        // 层二
        let comps = if tail.is_empty() {
            let lower = format!("{joined}'");
            self.scan_range(&lower, limit)
        } else if !joined.contains('\'') && limit <= TOPK {
            match self.topk.get(joined.as_str()) {
                Some(off) => self
                    .fetch_topk(off as usize, TOPK)
                    .into_iter()
                    .filter(|c| c.pinyin != joined)
                    .take(limit)
                    .collect(),
                None => self.scan_range(&joined, limit),
            }
        } else {
            // 全区间扫时跳过精确 key 本身：它已经在层一。
            self.scan_range_excluding(&joined, limit, Some(joined.as_bytes()))
        };
        (exact, comps)
    }

    /// 区间 `[prefix, increment_prefix(prefix))` 的全局 top-limit 归并。
    /// `prefix` 以字母结尾时 increment 必存在（store 版进位不返回 None）。
    fn scan_range(&self, prefix: &str, limit: usize) -> Vec<Candidate> {
        self.scan_range_excluding(prefix, limit, None)
    }

    fn scan_range_excluding(
        &self,
        prefix: &str,
        limit: usize,
        exclude: Option<&[u8]>,
    ) -> Vec<Candidate> {
        let upper = increment_prefix(prefix);
        let stream_builder = if let Some(up) = &upper {
            self.fst.range().ge(prefix).lt(up.as_str())
        } else {
            self.fst.range().ge(prefix)
        };
        let mut top: BinaryHeap<Worst> = BinaryHeap::with_capacity(limit.min(TOPK));
        let mut stream = stream_builder.into_stream();
        while let Some(out) = stream.next() {
            if exclude == Some(out.0) {
                continue; // 精确 key 归层一
            }
            self.collect_block(out.1, Some(out.0), &mut top, limit);
        }
        finish(top)
    }

    /// 声母缩写查询。单字母缩写的 key 集合与单字母前缀完全一致（abbrev 首字符 ≡ key 首字符），
    /// 直接查热表；更长的缩写走偏移表二分 + 顺序扫（同块头剪枝）。
    pub fn lookup_abbrev(&self, initials: &str, limit: usize) -> Vec<Candidate> {
        if limit == 0 || initials.is_empty() || !initials.bytes().all(|b| b.is_ascii_lowercase()) {
            return Vec::new();
        }
        // 单字母缩写的 key 集合与单字母前缀完全一致（abbrev 首字符 ≡ key 首字符）→ 查热表。
        // 更长的缩写两集合语义不同（"nh"=声母 n·h vs 前缀 "nh…"），必须走偏移表扫。
        if initials.len() == 1 && limit <= TOPK {
            if let Some(off) = self.topk.get(initials) {
                return self.fetch_topk(off as usize, limit);
            }
        }
        let mut top: BinaryHeap<Worst> = BinaryHeap::with_capacity(limit.min(TOPK));
        let mut i = self.abbrev_lower_bound(initials);
        while i < self.n_keys {
            let Some((name, key_idx)) = self.abbrev_entry(i) else {
                break;
            };
            if !name.starts_with(initials.as_bytes()) {
                break; // 表全局有序，越界即区间结束
            }
            self.collect_block(key_idx as u64, None, &mut top, limit);
            i += 1;
        }
        finish(top)
    }

    /// 首个 abbrev >= initials 的表项下标。
    fn abbrev_lower_bound(&self, initials: &str) -> usize {
        let (mut lo, mut hi) = (0usize, self.n_keys);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let name = match self.abbrev_entry(mid) {
                Some((n, _)) => n,
                None => return self.n_keys,
            };
            if name < initials.as_bytes() {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    fn abbrev_entry(&self, i: usize) -> Option<(&[u8], u32)> {
        let s = {
            let base = self.atbl_start + i * 4;
            let b = self._mmap.get(base..base + 4)?;
            u32::from_le_bytes(b.try_into().unwrap())
        };
        let e = {
            let base = self.atbl_start + (i + 1) * 4;
            let b = self._mmap.get(base..base + 4)?;
            u32::from_le_bytes(b.try_into().unwrap())
        };
        let rec = self
            ._mmap
            .get(self.arec_start + s as usize..self.arec_start + e as usize)?;
        let nlen = *rec.first()? as usize;
        let name = rec.get(1..1 + nlen)?;
        let idx_b: [u8; 4] = rec.get(1 + nlen..1 + nlen + 4)?.try_into().ok()?;
        Some((name, u32::from_le_bytes(idx_b)))
    }
}

/// top-k 堆适配器：堆顶恒为「当前最差的保留者」（freq 最小；同 freq 取 text 字典序最大）。
struct Worst(Candidate);

impl PartialEq for Worst {
    fn eq(&self, o: &Self) -> bool {
        self.0.freq == o.0.freq && self.0.text == o.0.text
    }
}
impl Eq for Worst {}
impl PartialOrd for Worst {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Worst {
    fn cmp(&self, o: &Self) -> Ordering {
        self.0
            .freq
            .cmp(&o.0.freq)
            .reverse()
            .then_with(|| self.0.text.cmp(&o.0.text))
    }
}

/// 堆 → 最终候选序 (freq DESC, text ASC, pinyin ASC)。pinyin 参与末位比较，
/// 保证跨块完全同分（freq+text 相同）时输出确定。
fn finish(top: BinaryHeap<Worst>) -> Vec<Candidate> {
    let mut candidates: Vec<Candidate> = top.into_iter().map(|w| w.0).collect();
    candidates.sort_by(|a, b| {
        b.freq
            .cmp(&a.freq)
            .then_with(|| a.text.cmp(&b.text))
            .then_with(|| a.pinyin.cmp(&b.pinyin))
    });
    candidates
}

/// 计算前缀的排他上界（末字符 +1）。'z' 进 '{'（紧随小写表之后），
/// 保证宽区间查询有界——v2 对 'z' 返回 None（开区间到 EOF），全量归并下代价不可接受。
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

/// 块内第一条（freq 最大者）的 (freq, text)；空块/畸形返回 None。用于堆满后的整块剪枝。
fn block_head(data: &[u8]) -> Option<(u64, &[u8])> {
    let count = block_count(data, 0)?;
    if count == 0 {
        return None;
    }
    let tl = text_len_at(data, COUNT_LEN)?;
    let s = COUNT_LEN + TEXT_LEN_LEN;
    let text = data.get(s..s + tl)?;
    let fb = data.get(s + tl..s + tl + FREQ_LEN)?;
    Some((u32::from_le_bytes(fb.try_into().unwrap()) as u64, text))
}

/// 解码 values 区一个块的前 `limit` 条。
///
/// 块在构建期已按 (freq DESC, text ASC) 排序，解到 limit 即停——v2 解整块
/// （`shen` 那种几千条）只为拿前 50，是每键 30–98ms 的根源。
/// 旧代码用「块首字节 == 0」判空块：计数换成 u32 LE 之后 256 条的块首字节同样是 0，
/// 该捷径会静默吞掉整块，故改为按 count 走。
fn decode_block(data: &[u8], pinyin: &str, limit: usize) -> Vec<Candidate> {
    let mut out = Vec::new();
    let Some(count) = block_count(data, 0) else {
        return out;
    };
    let take = count.min(limit);
    out.reserve(take);
    let mut pos = COUNT_LEN;
    for _ in 0..take {
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
