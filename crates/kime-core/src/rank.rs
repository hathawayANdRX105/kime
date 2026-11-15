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
//! 排序键是 `(tier, score DESC, text ASC, source)`：层在分数之前，所以「精确层整体
//! 在补全层之前」是**结构性事实**，任何频率、任何 LM 加成都无法把补全候选提到精确
//! 候选之前。这条契约历史上靠「先查精确块、再 append 补全块」的约定维持，任何一次
//! `sort`/`insert` 都可能打破；现在打破它需要显式改 [`Layer`] 序号。
//!
//! 语言不是排序维度。曾经试过「同语言比层、跨语言只比分」——那看着更「纯按分数」，
//! 但它不是全序：`(Zh,精确,10) < (Zh,补全,100) < (En,精确,50) < (Zh,精确,10)` 成环，
//! 比较器成环后 `sort_by` 的结果不再由输入唯一决定。跨语言竞争因此发生在**同层之内**：
//! 同为精确层时按校准分数交错，同为补全层时同理。
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
use std::collections::{HashMap, HashSet};

use crate::model::{Candidate, Lang, Layer, RankKey, SourceKind};

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

/// 跨语言分数校准：把各语言的原始频率映射到同一把尺上。
///
/// # 为什么必须校准
///
/// 中文词条频率来自音节统计（常见词 10⁵~10⁷），英文词频来自英文词表（常见词 10³ 量级）。
/// 直接比原始数值，中文永远压英文——那就退化成「中文整块在前」，只是换了个说法。
/// 校准后两边都是「在本语言内部的相对位置」，才谈得上「谁分高谁在前」。
///
/// # 用对数压缩
///
/// 频率分布是长尾的：线性归一化会让 99% 的词挤在 0 附近（最大值 10⁷ 时，
/// freq=10⁴ 只有 0.001）。对数压缩后 freq=10⁴ 与 10⁷ 的差距是 4/7 ≈ 0.57，
/// 与「数量级」的直觉一致。
///
/// # 边界
///
/// - `max` 为 0 或 1 时退化为线性（`eff` 直接当分数），避免除零与「全 0 分」。
/// - 分数上限 [`SCORE_MAX`]，留出与上下文加成相加的余量。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScoreScale {
    zh_max: u64,
    en_max: u64,
}

/// 分数上限：校准后的分数落在 `[0, SCORE_MAX]`。
pub const SCORE_MAX: u64 = 1_000_000_000;

/// 与上下文加成相加时的总上限。
const SCORE_CAP: u64 = u64::MAX / 2;

impl ScoreScale {
    /// 不做校准（分数 = 有效频率）。用于无词典上下文的场景（单测、诊断）。
    pub const IDENTITY: ScoreScale = ScoreScale {
        zh_max: 0,
        en_max: 0,
    };

    /// 用各语言的最大有效频率建尺。
    pub fn new(zh_max: u64, en_max: u64) -> Self {
        ScoreScale { zh_max, en_max }
    }

    /// 某语言某有效频率 → 可比分数。
    ///
    /// 单调性是硬要求：`eff_a > eff_b` 必须推出 `score_a >= score_b`。
    /// 所以这里**不截断到上界**——早期实现把 `eff` 截到 `max` 再归一化，
    /// 用户提频超过词表最大频时（ BOOST 叠加后常见）所有超额词并列满分，
    /// 排序退化成文本序，提频直接失效（`freq_boost_test` 钉死）。
    pub fn score(&self, lang: Lang, eff: u64) -> u64 {
        // 退化：任一语言没有频率数据（空词表 / 全 0 频）时校准无意义，
        // 直接比原始频率——对数会把 600010 与 500000 压成同一个整数 13，精度全丢。
        if self.zh_max <= 1 || self.en_max <= 1 {
            return eff.min(SCORE_CAP);
        }
        let k = self.factor(lang);
        let raw = (1.0 + eff as f64).ln() * k;
        if raw <= 0.0 {
            return 0;
        }
        if raw >= u64::MAX as f64 / 2.0 {
            return u64::MAX / 2;
        }
        raw as u64
    }

    /// 该语言的缩放系数。
    fn factor(&self, lang: Lang) -> f64 {
        let max = match lang {
            Lang::Zh => self.zh_max,
            Lang::En => self.en_max,
        };
        SCORE_MAX as f64 / (1.0 + max as f64).ln()
    }
}

/// 排序器：唯一比较入口。持有上下文加成来源与跨语言分数尺。
///
/// 构造开销为零（两个引用/一个 Copy 值），可在热路径上按需创建。
pub struct Ranker<'ctx> {
    boost: Option<&'ctx dyn CtxBoost>,
    scale: ScoreScale,
}

impl<'ctx> Ranker<'ctx> {
    /// 无上下文加成、无校准（分数 = 有效频率）。
    #[inline]
    pub fn plain() -> Ranker<'static> {
        Ranker {
            boost: None,
            scale: ScoreScale::IDENTITY,
        }
    }

    /// 带上下文加成的排序器。
    #[inline]
    pub fn with_boost(boost: &'ctx dyn CtxBoost) -> Ranker<'ctx> {
        Ranker {
            boost: Some(boost),
            scale: ScoreScale::IDENTITY,
        }
    }

    /// 带上下文加成 + 跨语言校准（词典查询的主路径）。
    #[inline]
    pub fn new(boost: &'ctx dyn CtxBoost, scale: ScoreScale) -> Ranker<'ctx> {
        Ranker {
            boost: Some(boost),
            scale,
        }
    }

    /// 校准尺（诊断/测试用）。
    #[inline]
    pub fn scale(&self) -> ScoreScale {
        self.scale
    }

    /// 比较键分数：校准后的有效频率 + 上下文加成，饱和。
    #[inline]
    pub fn score(&self, c: &Candidate) -> u64 {
        let base = self.scale.score(c.lang, c.eff);
        match self.boost {
            Some(b) => base.saturating_add(b.boost(c)).min(SCORE_CAP),
            None => base,
        }
    }

    /// 唯一比较器：`(tier, score DESC, text ASC, source ASC)`。
    ///
    /// 语言不是排序维度：见 [`RankKey`] 的注释——「层完整性 + 跨语言只比分 + 可传递」
    /// 三条不可兼得，这里保留层完整性，让跨语言竞争发生在同层之内。
    #[inline]
    pub fn compare(&self, a: &Candidate, b: &Candidate) -> Ordering {
        RankKey::of(a, self.score(a)).cmp(&RankKey::of(b, self.score(b)))
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

    /// 同一层内的批内重排：按分数降序、同分文本升序。
    ///
    /// 用于「同一层的多个来源合批」：双拼末键的声母位意图 + 韵母位意图、模糊音变体、
    /// 邻键纠错变体。这些批次在合批时统一标成同一层（见 `engine` 的半截键合批），
    /// 因此批内只比分就是正确语义，且结果可复现。
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
        RankKey::of(c, self.score(c))
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

/// 「已见文本」集合：跨多批候选共享的去重记录。
///
/// 存在的理由：engine 的多路查询（模糊音、邻键纠错、多切分、韵母位补全）各自产出
/// 一批候选，需要一个**共享的**已见集合才能跨批去重。旧实现每处手写一个
/// `HashSet<String>` + 一段 `retain`/`insert` 循环，保留规则与时机各不相同；现在
/// 集合与「追加」动作绑在一起，策略只能显式选择。
#[derive(Debug, Default)]
pub struct Seen {
    set: HashSet<String>,
}

impl Seen {
    pub fn new() -> Self {
        Self::default()
    }

    /// 用已有候选预填（新批次不得重复它们）。
    pub fn of<'a>(cands: impl IntoIterator<Item = &'a Candidate>) -> Self {
        Self {
            set: cands.into_iter().map(|c| c.text.clone()).collect(),
        }
    }

    /// 记录一条文本；首次出现返回 `true`。
    pub fn insert(&mut self, text: &str) -> bool {
        self.set.insert(text.to_string())
    }

    pub fn contains(&self, text: &str) -> bool {
        self.set.contains(text)
    }
}

/// 追加一批候选，跨批按文本去重（保留先出现者）。
///
/// `seen` 由调用方持有，因此同一轮按键里的多批查询共享同一份记录——这是「先到
/// 先得」语义（首切分优先于模糊音/纠错变体）唯一正确的表达方式。
pub fn append_unique(cands: &mut Vec<Candidate>, batch: Vec<Candidate>, seen: &mut Seen) {
    for c in batch {
        if seen.insert(&c.text) {
            cands.push(c);
        }
    }
}

/// 把一批候选并入已有列表：位置以已有列表为准，同文本按 `policy` 决定是否替换载荷。
///
/// 用途：多切分合并（同文本保留频率更高的出现，但位置留在首次出现处）、用户词
/// overlay 合并。与 [`dedup`] 的区别是它**保留已有顺序**而不是重新收集。
pub fn merge_unique(
    ranker: &Ranker<'_>,
    base: Vec<Candidate>,
    extra: Vec<Candidate>,
    policy: Dedup,
) -> Vec<Candidate> {
    let mut out = base;
    let mut pos: HashMap<String, usize> = out
        .iter()
        .enumerate()
        .map(|(i, c)| (c.text.clone(), i))
        .collect();
    for c in extra {
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

/// 过滤掉 `seen` 中已存在的文本，并把保留下来的记进 `seen`。
///
/// 用途：候选池需要「与主列表不重复」时使用（首音节单字追加、句子落位）。
pub fn retain_unseen(cands: &mut Vec<Candidate>, seen: &mut Seen) {
    cands.retain(|c| seen.insert(&c.text));
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
    mut head: Vec<Candidate>,
    mut append: Vec<Candidate>,
) -> Vec<Candidate> {
    // 归并前提是两段都按分数降序。调用方给的 head 是主列表去掉保护首段后的尾部
    // （层优先拼接，非分数序），append 是查询结果直接打标（同样非分数序）——
    // 不预排序的话，同一多重集换个输入顺序就换个交错结果。
    ranker.sort_within_batch(&mut head);
    ranker.sort_within_batch(&mut append);
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
