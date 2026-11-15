//! 用户学习统计 —— 中英文共用的一套计数与加成算式。
//!
//! # 设计要点（为什么要单独一层）
//!
//! ## 1. 两套学习入口，一套语义
//!
//! 旧实现里中文走 `learn_mem`（写 phrase 表 + 旁表）、英文走 `learn_english`
//! （只写旁表），两条路径各自维护计数与烘焙。它们共享同一个加成算式，却在
//! **键空间**上靠约定区分：中文键 = `(读音, 文本)`，英文键 = `(词, 词)`。
//!
//! 靠「读音 == 文本」区分中英文是脆弱判据：它依赖「汉字永不等于 ASCII 拼音」这条
//! 隐含假设，一旦英文导入规则或键设计变化，英文计数就会误触中文拼音块的重排，
//! 破坏「英文学习不改变中文候选顺序」这条契约。
//!
//! 现在键是**带语言标签的枚举**（[`LearnKey`]）：语言是键的类型的一部分，不是
//! 从内容猜出来的。
//!
//! ## 2. 持久化格式不变
//!
//! 落盘仍是 `kime_kv` 里 `key = pinyin \t text`、`value = "n,day"`（历史格式，
//! 已有用户数据必须能读回）。语言标签只在内存中重建：读回时按
//! [`LearnKey::from_storage`] 分类，写出去时按 [`LearnKey::to_storage`] 还原。
//! 于是「英文绝不进中文词表」这条契约由类型保证，而磁盘格式零迁移。
//!
//! ## 3. 加成只影响排序
//!
//! [`UserStats::bonus`] 返回的加成只进比较键（`Candidate::eff` 的烘焙值），
//! 永不写回 `freq`。半衰期衰减在查询/烘焙时现算，因此「90 天不用自动回落」
//! 不需要后台任务。

use std::collections::{HashMap, HashSet};

/// 用户学习键：语言是键的一部分，而不是从字符串内容猜出来的。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LearnKey {
    /// 中文词条：读音（音节 `'` 连接）+ 上屏文本。
    Zh { pinyin: String, text: String },
    /// 英文词：词本身（词库写法，大小写敏感）。
    En { word: String },
}

impl LearnKey {
    /// 落盘键（`kime_kv` 历史格式：`pinyin \t text`）。
    ///
    /// 英文的 `pinyin` 位填词本身——与旧实现的英文约定逐字节一致，因此既有
    /// 用户数据可直接读回。
    pub fn to_storage(&self) -> (String, String) {
        match self {
            LearnKey::Zh { pinyin, text } => (pinyin.clone(), text.clone()),
            LearnKey::En { word } => (word.clone(), word.clone()),
        }
    }

    /// 从落盘键重建语言标签。
    ///
    /// 分类判据与旧实现的英文约定一致（两段相等 ⇒ 英文），但**只在这一处**出现，
    /// 且注释写明它为何安全：英文词表只收纯 ASCII 字母词（`import_english` 强制），
    /// 而中文读音/文本来自 phrase 表（汉字），两段不可能相等。
    pub fn from_storage(pinyin: String, text: String) -> Self {
        if pinyin == text && pinyin.bytes().all(|b| b.is_ascii_alphabetic()) {
            LearnKey::En { word: text }
        } else {
            LearnKey::Zh { pinyin, text }
        }
    }

    /// 是否是中文键（决定它能否触发中文拼音块的 `eff` 重排）。
    pub fn is_zh(&self) -> bool {
        matches!(self, LearnKey::Zh { .. })
    }
}

/// 用户学习统计：键 → (使用次数, 最近使用日)。
#[derive(Debug, Default, Clone)]
pub struct UserStats {
    inner: HashMap<LearnKey, (u64, u64)>,
}

impl UserStats {
    /// 从 `(落盘键 → 计数)` 载入（开库时调用一次）。
    pub fn from_storage<I>(rows: I) -> Self
    where
        I: IntoIterator<Item = ((String, String), (u64, u64))>,
    {
        let inner = rows
            .into_iter()
            .map(|(k, v)| (LearnKey::from_storage(k.0, k.1), v))
            .collect();
        Self { inner }
    }

    /// 落盘视图（后台写线程按绝对计数收敛，见 `learn_writer`）。
    pub fn to_storage(&self) -> impl Iterator<Item = ((String, String), (u64, u64))> + '_ {
        self.inner.iter().map(|(k, v)| (k.to_storage(), *v))
    }

    /// 读计数（诊断/测试用）。
    pub fn count_of(&self, key: &LearnKey) -> Option<(u64, u64)> {
        self.inner.get(key).copied()
    }

    /// 记录一次使用，返回新的累计次数。
    pub fn record(&mut self, key: LearnKey, today: u64) -> u64 {
        let n = self
            .inner
            .get(&key)
            .map(|(n, _)| n.saturating_add(1))
            .unwrap_or(1);
        self.inner.insert(key, (n, today));
        n
    }

    /// 提频加成：`n × USER_BOOST × 0.5^(age / 半衰期)`。
    ///
    /// 无记录 → 0；`n = 1` 即满额（首用即加成）。纯函数（除查表），
    /// 开库烘焙与学习当帧生效共用这一条算式——**没有第二份语义**。
    pub fn bonus_of(&self, today: u64, pinyin: &str, text: &str) -> u64 {
        bonus_of(&self.inner, today, pinyin, text)
    }

    /// 是否有该读音的中文学习记录（决定是否需要在层一按需重排 `eff`）。
    ///
    /// 只看中文键：英文计数不得触发中文块重排（`english_learning_test::A6`）。
    pub fn has_zh_pinyin(&self, pinyin: &str) -> bool {
        self.inner.keys().any(|k| match k {
            LearnKey::Zh { pinyin: p, .. } => p == pinyin,
            LearnKey::En { .. } => false,
        })
    }

    /// 有学习记录的读音集合（中文键的读音投影）。
    pub fn zh_pinyins(&self) -> HashSet<&str> {
        self.inner
            .keys()
            .filter_map(|k| match k {
                LearnKey::Zh { pinyin, .. } => Some(pinyin.as_str()),
                LearnKey::En { .. } => None,
            })
            .collect()
    }

    /// 底层只读视图（老调用点按 `(读音, 文本)` 现算加成时使用）。
    pub fn as_map(&self) -> &HashMap<LearnKey, (u64, u64)> {
        &self.inner
    }
}

/// 用户词提频（第五轮调频方案 B：使用即提升 + 时间衰减，rime user_freq 同款语义）。
///
/// 语料计数是 10⁵~10⁷ 量级（常见词「我们」freq=509,405），旧版 learn 一次 +1
/// 对排序毫无作用。第 n 次使用给该词叠加 `n × USER_BOOST × 0.5^(age/半衰期)`
/// 的**排序用**有效频率。首用即满额是用户拍板（2026-09-15）：rime 手感——
/// 选过的词立刻要有存在感；`tests/dict.rs` 钉「等量加成相抵、排序仍由裸频定」。
///
/// 校准：BOOST=300_000 ⇒ 第 1 次使用（300k）压过 10⁵ 级长尾词、压不过 50 万级
/// 语料词；第 2 次（600k）压过 freq=500,000 的普通语料词。
pub const USER_BOOST: u64 = 300_000;

/// 提频半衰期（天）：连续 30 天不再使用，加成减半；60 天降到 1/4，回落语料位。
pub const USER_BOOST_HALF_LIFE_DAYS: f64 = 30.0;

/// 某词的提频加成（自由函数形式，供开库烘焙与学习当帧生效共用）。
pub fn bonus_of(
    stats: &HashMap<LearnKey, (u64, u64)>,
    today: u64,
    pinyin: &str,
    text: &str,
) -> u64 {
    let key = LearnKey::from_storage(pinyin.to_string(), text.to_string());
    let Some(&(n, last)) = stats.get(&key) else {
        return 0;
    };
    let age = today.saturating_sub(last) as f64;
    let factor = 0.5f64.powf(age / USER_BOOST_HALF_LIFE_DAYS);
    (n as f64 * USER_BOOST as f64 * factor) as u64
}
