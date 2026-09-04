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

use std::path::Path;

use rusqlite::Result;

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

pub struct Dict;

impl Dict {
    /// 打开；不存在则建 schema + 索引
    pub fn open(_path: impl AsRef<Path>) -> Result<Self> {
        todo!("M1: 建表建索引 + PRAGMA synchronous=NORMAL")
    }

    /// 导入 rime-ice `.dict.yaml`：解析 TSV 正文（文字\t拼音\t频率），
    /// 幂等（重复行跳过），返回新增条数
    pub fn import(&mut self, _dict_yaml: impl AsRef<Path>) -> Result<usize> {
        todo!("M1")
    }

    /// 精确查询：读音序列 → 候选，freq 降序 LIMIT limit
    pub fn lookup(&self, _reading: &[String], _limit: usize) -> Result<Vec<Candidate>> {
        todo!("M1")
    }

    /// 学习：用户选定 (读音, 词) → bump 用户词频 / 插入用户词
    pub fn learn(&mut self, _reading: &[String], _text: &str) -> Result<()> {
        todo!("M2")
    }
}
