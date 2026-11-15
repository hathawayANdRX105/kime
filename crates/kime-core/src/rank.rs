//! 排序内核 —— 候选顺序的**唯一**真实来源。
//!
//! # 设计要点（为什么集中成一个模块）
//!
//! ## 1. 一个比较器
//!
//! 旧实现里同一语义有三份实现：词典条目的 `(读音, 频率, 文本)` 比较器、候选的
//! `(有效频率 + LM 加成, 文本)` 比较器、engine 双拼分支里内联的第三份「有效频率 +
//! LM 加成」比较。三份实现作用域不同却都在决定候选顺序，任何一次调参都要求三处
//! 同步修改，漏一处就是排序分歧。现在所有比较只走 [`Ranker::compare`]。
//!
//! ## 2. 层完整性由排序键保证，而不是由调用顺序保证
//!
//! 排序键高位是 `(lang, layer)`，分数在第三位。因此「精确层整体在补全层之前」是
//! 一个**结构性事实**：任何有效频率、任何 LM 加成都无法把补全候选提到精确候选
//! 之前。这条契约历史上靠「先查精确块、再 append 补全块」的约定维持，任何一次
//! `sort`/`insert` 都可能打破；现在打破它需要显式改 [`Layer`] 序号。
//!
//! ## 3. 上下文加成是**比较时**的量，不污染导出值
//!
//! 用户学习加成在开库/学习时烘焙进 [`Candidate::eff`]（持久化、可恢复）；LM 上下文
//! 加成只在本次查询期间有效，因此不写入 `eff`——`eff` 是「裸频 + 用户加成」这一
//! 可断言、可导出的量（`english_learning_test` 钉 `eff == freq + 用户加成`）。
//! 比较器通过 [`CtxBoost`] 把它加在比较键上，默认实现为零加成。
//!
//! ## 4. 溢出饱和
//!
//! 比较键用 `u64` 饱和加（`saturating_add`）。旧实现是 `eff as i64 + lm_boost`：
//! `u64 → i64` 的 `as` 转换在超过 `i64::MAX` 时按二进制截断成负数，会静默反转
//! 高频段的顺序。现在全程无符号、饱和，不存在回绕。
//!
//! ## 5. 去重是显式的策略，不是各处随手写的 `HashSet`
//!
//! 旧实现里模糊音、纠错、多切分、组句、AI 各维护一份 seen 集合，保留规则各不
//! 相同（有的保留首个、有的保留频率最高）。这里把「保留谁」变成参数：
//! [`Dedup::FirstWins`]（既有首现优先语义）、[`Dedup::HighestEff`]（保留频率更高
//! 的出现）、[`Dedup::BestRank`]（保留排序键更优者）。去重结果**位置稳定**：保留
//! 首次出现的位置，只替换载荷——用 `HashMap` 收集后 `into_values()` 会丢顺序。
//!
//! ## 6. 首音节单字是唯一允许的跨层归并
//!
//! 契约要求首音节单字按有效频率与主列表双列合并（高频首字要能越过弱补全），这是
//! 唯一一处「分数可以跨越层边界」的地方。它因此单独建模为 [`Layer::Append`]，
//! 只能通过 [`merge_append`] 归并，不进入 [`Ranker::sort`] 的整表排序——否则
//! 「精确层不可被跨层超越」这条不变量会出现一个没人记得的例外。
//!
//! ## 7. 无分配比较
//!
//! [`RankKey`] 借用候选字段（`&str`），排序过程零分配。热路径是每次按键一次候选
//! 排序（数十条），分配次数在这个量级上可测量。

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::model::{Candidate, Layer, RankKey, SourceKind};

/// 排序时叠加的上下文加成（LM bigram 先验）。
///
/// 与用户学习加成不同：用户加成烘焙进 `Candidate.eff`（持久化、跨会话），上下文
/// 加成是本次查询的临时量（同一候选在不同上文下分数不同），因此只在这里出现。
/// 默认零加成，保证「没有上下文时排序退化为纯有效频率」。
pub trait CtxBoost {
    fn boost(&self, c: &Candidate) -> u64;
}

/// 零上下文加成（单测、无 LM 场景）。
#[derive(Clone, Copy, Debug, Default)]
pub struct NoBoost;

impl CtxBoost for NoBoost {
    #[inline]
    fn boost(&self, _c: &Candidate) -> u64 {
        0
    }
}

/// 排序器：唯一比较入口。持有可选的上下文加成来源。
///
/// 构造开销为零（只存一个引用），可在热路径上按需创建。
pub struct Ranker<'ctx> {
    boost: Option<&'ctx dyn CtxBoost>,
}

/// 比较键的上限：有效频率 + 上下文加成不得超过此值，留出加法余量。
const SCORE_CAP: u64 = u64::MAX / 2;

impl<'ctx> Ranker<'ctx> {
    /// 无上下文加成的排序器（纯有效频率序）。
    #[inline]
    pub fn plain() -> Ranker<'static> {
        Ranker { boost: None }
    }

    /// 带上下文加成的排序器。
    #[inline]
    pub fn with_boost(boost: &'ctx dyn CtxBoost) -> Ranker<'ctx> {
        Ranker { boost: Some(boost) }
    }

    /// 比较键分数：有效频率 + 上下文加成，饱和。
    #[inline]
    pub fn score(&self, c: &Candidate) -> u64 {
        match self.boost {
            Some(b) => c.eff.saturating_add(b.boost(c)).min(SCORE_CAP),
            None => c.eff,
        }
    }

    /// 唯一比较器：`(lang, layer, score DESC, text ASC, source ASC)`。
    #[inline]
    pub fn compare(&self, a: &Candidate, b: &Candidate) -> Ordering {
        let (ka, kb) = (self.score(a), self.score(b));
        a.lang
            .cmp(&b.lang)
            .then_with(|| a.layer.cmp(&b.layer))
            // 层内：分数降序
            .then_with(|| kb.cmp(&ka))
            // 同分：文本升序（与输入顺序无关，保证结果可复现）
            .then_with(|| a.text.cmp(&b.text))
            // 同文本同分：来源优先级
            .then_with(|| a.source.cmp(&b.source))
    }

    /// 稳定排序（Rust `sort_by` 稳定：同键保持输入相对顺序）。
    pub fn sort(&self, cands: &mut [Candidate]) {
        cands.sort_by(|a, b| self.compare(a, b));
    }

    /// 只在**同批候选内**按分数降序重排，不改变层归属。
    ///
    /// 用于层二补全、英文块、用户词 overlay 这类「批内有序、批间不许打平」的场景。
    /// 层间顺序由 [`Ranker::compare`] 负责，本函数不参与。
    pub fn sort_within_batch(&self, cands: &mut [Candidate]) {
        cands.sort_by(|a, b| {
            self.score(b)
                .cmp(&self.score(a))
                .then_with(|| a.text.cmp(&b.text))
                .then_with(|| a.source.cmp(&b.source))
        });
    }

    /// **跨查询意图**合批重排：只按分数降序、同分文本升序，**不看层**。
    ///
    /// 为什么需要一个「不看层」的排序：层是「单次查询内部」的概念——一次查询的精确块
    /// 与另一次查询的补全块之间，层号没有可比性。engine 把多次查询的结果合成一批时
    /// （双拼末键的声母位意图 + 韵母位意图、模糊音变体、邻键纠错变体），若按层排序，
    /// 就会让「韵母位查询的精确命中」压过「声母位查询的补全命中」，而后者才是用户按
    /// 键的直接意图（`pending_shuangpin_key_test` 钉死：sh 系词必须在 wu 系词之前）。
    ///
    /// 因此跨意图合批只允许按分数重排；层完整性只在单次查询的结果内部成立。
    pub fn sort_by_score(&self, cands: &mut [Candidate]) {
        cands.sort_by(|a, b| {
            self.score(b)
                .cmp(&self.score(a))
                .then_with(|| a.text.cmp(&b.text))
                .then_with(|| a.source.cmp(&b.source))
        });
    }

    /// 排序键视图（诊断/测试用）。
    pub fn key<'a>(&self, c: &'a Candidate) -> RankKey<'a> {
        RankKey {
            lang: c.lang,
            layer: c.layer,
            eff: self.score(c),
            text: c.text.as_str(),
            source: c.source,
        }
    }
}

/// 同文本去重的保留策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dedup {
    /// 保留首次出现者（既有语义：模糊音/纠错/多切分的「先到先得」）。
    FirstWins,
    /// 保留有效频率更高者（既有语义：多切分合并时同文本取最高频）。
    HighestEff,
    /// 保留排序键更优者（层更高、同层分数更高）。
    BestRank,
}

/// 按文本去重，位置稳定：保留首次出现的位置，只按策略替换载荷。
///
/// 位置稳定是刻意的——候选顺序必须是输入顺序的确定性函数。旧实现用 `HashMap`
/// 收集后 `into_values()`，同一次查询可能给出不同顺序。
pub fn dedup(ranker: &Ranker<'_>, cands: Vec<Candidate>, policy: Dedup) -> Vec<Candidate> {
    let mut pos: HashMap<String, usize> = HashMap::with_capacity(cands.len());
    let mut out: Vec<Candidate> = Vec::with_capacity(cands.len());
    for c in cands {
        match pos.get(c.text.as_str()).copied() {
            None => {
                pos.insert(c.text.clone(), out.len());
                out.push(c);
            }
            Some(i) => {
                let replace = match policy {
                    Dedup::FirstWins => false,
                    Dedup::HighestEff => c.eff > out[i].eff,
                    Dedup::BestRank => ranker.compare(&c, &out[i]) == Ordering::Less,
                };
                if replace {
                    out[i] = c;
                }
            }
        }
    }
    out
}

/// 是否是首音节单字追加候选（唯一允许跨层归并的来源）。
#[inline]
pub fn is_append(c: &Candidate) -> bool {
    c.layer == Layer::Append
}

/// 首音节单字双列归并：按分数降序交错，同分时主列表优先。
///
/// 契约（`first_syllable_full_list_test`）：
///
/// - 主列表已有的候选不被追加项挤掉相对顺序（同分主列表优先）；
/// - 追加项按有效频率插到主列表正确位置（#87：高频首字要能越过弱补全）；
/// - 主列表头部（精确块 / 覆盖句）受保护——调用方只把「头部之后的候选」传进来。
pub fn merge_append(
    ranker: &Ranker<'_>,
    head: Vec<Candidate>,
    append: Vec<Candidate>,
) -> Vec<Candidate> {
    let mut head = head.into_iter().peekable();
    let mut append = append.into_iter().peekable();
    let mut out: Vec<Candidate> = Vec::with_capacity(head.len() + append.len());
    loop {
        // peek 的借用必须在 next() 之前结束，否则迭代器会被移动；
        // 因此先在 match 里只算「该取哪一边」，再统一 next()。
        let take_head = match (head.peek(), append.peek()) {
            (Some(h), Some(a)) => ranker.score(h) >= ranker.score(a),
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        if take_head {
            out.push(head.next().expect("peeked head"));
        } else {
            out.push(append.next().expect("peeked append"));
        }
    }
    out
}

/// 层二补全的名额保留：层一（精确）最多占用 `limit - reserve`。
///
/// 契约（`candidate_layers_test` / `lookup_exact_test`）：tail 非空时层一可能有
/// 几百条精确候选，若不预留名额，`mi` 这类输入永远够不到 `min`/`ming` 的字。
/// tail 为空（音节已收口）时层一独占，不做预留——调用方传 `reserve = 0`。
pub fn reserve_for_completion(limit: usize) -> usize {
    limit.div_ceil(2)
}

/// 层一预算 = 总名额 − 补全保留名额。
pub fn exact_budget(limit: usize, reserve: usize) -> usize {
    limit.saturating_sub(reserve)
}

/// 候选是否属于「词典精确块」：层为精确且来源是词典 / 用户词 / 英文词条。
///
/// engine 的句子落位与首字归并的头部保护都用这条判据，替代旧实现里
/// `take_while(|c| c.pinyin == l1_key)` 这种依赖读音字符串的约定。
pub fn in_exact_block(c: &Candidate) -> bool {
    c.layer == Layer::Exact
        && matches!(
            c.source,
            SourceKind::Dict | SourceKind::User | SourceKind::English
        )
}

/// 候选是否属于「主列表」：除首字追加之外的所有层。
pub fn in_main_list(c: &Candidate) -> bool {
    c.layer != Layer::Append
}
