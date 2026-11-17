//! 语料加载：读取 TSV 行（`freq\ttext\tpinyin`），按 zh/en 分桶，支持从 fixture 生成。
//!
//! 格式与根 `corpus-dump.tsv` 同式，三列制表符分隔，无引号转义歧义，易手写。
//! 英文行的 `pinyin` 列填空串；lang 标记由文件名前缀（`zh_` / `en_`）承担。

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// 单条语料条目。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CorpusEntry {
    /// 条目 id（通常为句子编号，也用于 lang 分桶推断）
    pub id: String,
    /// 词频（语料侧先验，不参与本 crate 排序）
    pub freq: u64,
    /// 目标文本（中文句子或英文句子）
    pub text: String,
    /// 拼音（音节 `'` 连接，如 "ni'hao"；英文行为空串）
    pub pinyin: String,
}

impl CorpusEntry {
    /// 是否中文条目：读音非空即中文（英文行 `pinyin` 留空）。
    ///
    /// 全 crate 唯一的语言判据，`runner::Lang` 也按它推断，避免两处口径漂移。
    pub fn is_zh(&self) -> bool {
        !self.pinyin.is_empty()
    }
}

/// 语料集合：按加载时原序保持，lang 由条目 id/文件名推断。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Corpus {
    pub entries: Vec<CorpusEntry>,
}

impl Corpus {
    /// 从 TSV 文件加载语料。
    ///
    /// 每行两到三列：`freq`、`text`、可选 `pinyin`（英文行省略）。
    /// 空行与 `#` 开头的注释行被跳过；少于两列的行视为尾部损坏行，静默跳过。
    pub fn load_tsv(path: &Path) -> anyhow::Result<Self> {
        let file = fs::File::open(path).map_err(|e| anyhow::anyhow!("open corpus: {e}"))?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        for (line_no, line) in reader.lines().enumerate() {
            let line = line.map_err(|e| anyhow::anyhow!("read corpus line {line_no}: {e}"))?;
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let cols: Vec<&str> = trimmed.split('\t').collect();
            if cols.len() < 2 {
                continue;
            }
            let freq: u64 = cols[0]
                .parse()
                .map_err(|e| anyhow::anyhow!("parse freq at line {line_no}: {e}"))?;
            entries.push(CorpusEntry {
                id: format!("{}:{}", path.display(), line_no + 1),
                freq,
                text: cols[1].to_string(),
                pinyin: cols.get(2).copied().unwrap_or("").to_string(),
            });
        }
        Ok(Self { entries })
    }

    /// 中文条目：**读音非空**即中文。
    ///
    /// 判据是数据而不是文件名——英文行的 `pinyin` 列留空（见模块文档），
    /// 中文行必带拼音。曾经按 id 里的 `zh` 子串判，与 `runner::Lang` 的
    /// 「文件名以 zh 开头」口径不一致，真实语料（id 带完整路径）全部落进 en 桶。
    pub fn zh_entries(&self) -> Vec<&CorpusEntry> {
        self.entries.iter().filter(|e| e.is_zh()).collect()
    }

    /// 英文条目。
    pub fn en_entries(&self) -> Vec<&CorpusEntry> {
        self.entries.iter().filter(|e| !e.is_zh()).collect()
    }

    /// 条目总数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 从内嵌字符串构造 Corpus（测试用，避免依赖真实文件系统）。
pub fn corpus_from_tsv(content: &str) -> Corpus {
    let mut entries = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = trimmed.split('\t').collect();
        if cols.len() < 2 {
            continue;
        }
        let freq: u64 = cols[0].parse().unwrap_or(1);
        entries.push(CorpusEntry {
            id: format!("embedded:{i}"),
            freq,
            text: cols[1].to_string(),
            pinyin: cols.get(2).copied().unwrap_or("").to_string(),
        });
    }
    Corpus { entries }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_tsv_parses_zh_en() {
        let content = "1\t我们\two'men\n2\thello\n3\t北京\tbei'jing\n";
        let corpus = corpus_from_tsv(content);
        assert_eq!(corpus.len(), 3);
        assert_eq!(corpus.zh_entries().len(), 2);
        assert_eq!(corpus.en_entries().len(), 1);
        assert_eq!(corpus.entries[0].text, "我们");
        assert_eq!(corpus.entries[0].pinyin, "wo'men");
        assert_eq!(corpus.entries[1].text, "hello");
        assert_eq!(corpus.entries[1].pinyin, "");
    }

    #[test]
    fn load_tsv_skips_comments_and_blank() {
        let content = "# comment\n\n1\t我们\two'men\n";
        let corpus = corpus_from_tsv(content);
        assert_eq!(corpus.len(), 1);
    }
}
