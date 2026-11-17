//! 语料加载：读取 TSV 行（`freq\ttext\tpinyin`），按 zh/en 分桶。
//!
//! 格式与根 `corpus-dump.tsv` 同式，三列制表符分隔，无引号转义歧义，易手写。
//! 英文行的 `pinyin` 列填空串；lang 标记由条目读音是否为空承担（见 [`CorpusEntry::is_zh`]）。
//!
//! # 文章分组
//!
//! `# article: <标题>` 注释行开启一篇文章，其后的条目 id 变成 `<标题>#<行号>`。
//! 这样一份 TSV 能同时装多篇文章，报告里靠 id 前缀就能看出某条属于哪个文章——
//! 语料是**词表按文章顺序排列**（每条一个词），因为 `top1_hit` 这类指标是按词定义的
//! （见 [`crate::metrics::SentenceMetric`]）。

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
    /// 空行与 `#` 开头的注释行被跳过（`# article: <标题>` 除外，它开启一篇文章）；
    /// 少于两列的行视为尾部损坏行，静默跳过。
    pub fn load_tsv(path: &Path) -> anyhow::Result<Self> {
        let file = fs::File::open(path).map_err(|e| anyhow::anyhow!("open corpus: {e}"))?;
        let reader = BufReader::new(file);
        let mut content = String::new();
        for (line_no, line) in reader.lines().enumerate() {
            let line = line.map_err(|e| anyhow::anyhow!("read corpus line {line_no}: {e}"))?;
            content.push_str(&line);
            content.push('\n');
        }
        Ok(Self {
            entries: parse_entries(&content, &path.display().to_string())?,
        })
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

/// 解析 TSV 文本：文件加载与内嵌字符串（测试）共用同一份实现，避免两处口径漂移。
///
/// `source` 是无文章标题时的 id 前缀（文件路径或 `embedded`）。遇到
/// `# article: <标题>` 后，条目 id 变成 `<标题>#<行号>`。
fn parse_entries(content: &str, source: &str) -> anyhow::Result<Vec<CorpusEntry>> {
    let mut entries = Vec::new();
    let mut article: Option<String> = None;
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('#') {
            // `# article: <标题>` 开启一篇文章；其它注释行忽略。
            let title = rest
                .trim()
                .strip_prefix("article:")
                .map(str::trim)
                .filter(|t| !t.is_empty());
            if let Some(title) = title {
                article = Some(title.to_string());
            }
            continue;
        }
        let cols: Vec<&str> = trimmed.split('\t').collect();
        if cols.len() < 2 {
            continue;
        }
        let freq: u64 = cols[0]
            .parse()
            .map_err(|e| anyhow::anyhow!("parse freq at line {}: {e}", i + 1))?;
        let id = match &article {
            Some(title) => format!("{title}#{}", i + 1),
            None => format!("{source}:{}", i + 1),
        };
        entries.push(CorpusEntry {
            id,
            freq,
            text: cols[1].to_string(),
            pinyin: cols.get(2).copied().unwrap_or("").to_string(),
        });
    }
    Ok(entries)
}

/// 从内嵌字符串构造 Corpus（测试用，避免依赖真实文件系统）。
pub fn corpus_from_tsv(content: &str) -> Corpus {
    Corpus {
        entries: parse_entries(content, "embedded").expect("embedded corpus must parse"),
    }
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
