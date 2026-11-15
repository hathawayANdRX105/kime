//! 候选模型 —— 全链路唯一的「候选货币」与排序语义定义处。
//!
//! # 设计要点（为什么是现在这个样子）
//!
//! ## 1. 语言、层、来源是**数据**，不是推断
//!
//! 旧实现把「这条候选属于精确层还是补全层」「它是中文还是英文」都编码在
//! *位置*（先 push 的在前）与 *读音字段*（英文候选把词本身塞进读音字段）里。
//! 后果：
//!
//! - 判断层归属要回看插入顺序，任何一处 `insert`/`extend` 都可能静默改层；
//! - 英文候选的层判断寄生在「读音字段 == 按键串」这条隐含约定上；
//! - 想表达「中文整体先于英文」只能整块拼接，无法与层规则组合。
//!
//! 现在每条候选自携 [`Lang`] / [`Layer`] / [`SourceKind`]，层与语言不再从位置或
//! 字段内容反推。新增来源只需给出这三者的静态取值。
//!
//! ## 2. `pinyin` 是**读音键**，语义由 [`Lang`] 决定
//!
//! 中文：音节 `'` 连接（`ni'hao`）；英文：词本身（`AA`）。字段名沿用历史名称以
//! 兼容持久化与学习键，但**解释权归 [`Lang`]**：任何跨语言判断必须先看 `lang`，
//! 禁止再用字符串内容猜语言。
//!
//! ## 3. 原始频率与有效频率分离
//!
//! [`Candidate::freq`] 是库内原始值，是对外导出与持久化的契约，**永不被学习加成
//! 污染**；[`Candidate::eff`] 是排序用的有效频率（语料频 + 用户学习加成，开库/
//! 学习时烘焙）。两者分开的理由：导出值一旦被加成改写，用户数据就不可逆地丢失
//! 「这个词原本多少频」这一事实。
//!
//! ## 4. 层是排序键的高位字段，分数只在同一层内比较
//!
//! 见 [`crate::rank`]：排序键为 `(lang, layer, eff DESC, text ASC, source)`。
//! 层号在分数之前比较，因此**任何分数都无法让候选跨层跳跃**——这正是
//! 「精确命中必须整体在补全之前，哪怕补全频率高一个数量级」这条契约的
//! 结构性保证，而不是靠调用顺序维持的约定。

use std::cmp::Ordering;

/// 候选语言。排序序即语言优先级：`Zh` 整体先于 `En`（用户裁定）。
///
/// 语言顺序是一条**显式声明的策略**，不是分数比较的结果。理由：中文有效频率来自
/// 音节词条统计，英文有效频率来自英文词频统计，两者统计空间与量级不同，没有共享
/// 校准数据；把它们放进同一个数轴比较，等于引入一个无法解释的缩放常量，且会让
/// 高频英文词挤进中文候选前列（历史 bug 形态）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Lang {
    /// 中文（拼音/双拼词条、句子组词、声母缩写、首字回退）
    Zh = 0,
    /// 英文（原始按键串匹配的英文词）
    En = 1,
}

impl Lang {
    /// 由读音键的语言归属反查，仅在构造候选时使用；比较路径不得靠字符串猜语言。
    pub fn of_english(is_english: bool) -> Self {
        if is_english {
            Lang::En
        } else {
            Lang::Zh
        }
    }
}

/// 候选层级。**层序即排序序**（数字小者优先），层内才允许比较有效频率。
///
/// 层序为何是这样排（每一条都对应一条已固定的可观测契约）：
///
/// 1. [`Layer::Exact`] —— 词典精确命中（读音等于输入）。必须最前：打 `min` 时
///    「民/敏/抿」必须压过频率高一个数量级的「明/名/命」。
/// 2. [`Layer::SentenceCovers`] —— 覆盖完整输入的组句候选。落在精确块之后、
///    补全之前：整句不能压过真词，但要比碎切补全更贴近「用户想要整句」。
/// 3. [`Layer::Completion`] —— 词典补全（输入是读音前缀）。
/// 4. [`Layer::Tail`] —— 未覆盖完整输入的组句候选，落列表尾（半截键场景下
///    匹配半截声母的补全必须排在只覆盖已完成音节的句之前）。
/// 5. [`Layer::Fallback`] —— 兜底候选（最长前缀回退 / 声母缩写）：只在主查询
///    无结果时出现，层号仅用于保证它们不会插到正常候选之前。
///
/// [`Layer::Append`] 不在上表：首音节单字是**唯一允许跨层按有效频率归并**的
/// 来源（见 [`crate::rank::merge_append`]），它在类型上单独标记，避免被误当成
/// 普通层参与整表排序。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    /// 词典精确命中（中文读音等于输入 / 英文按键串等于词，大小写不敏感）
    Exact = 0,
    /// 覆盖完整输入的组句候选
    SentenceCovers = 1,
    /// 词典补全（中文跨音节边界 / 英文前缀）
    Completion = 2,
    /// 未覆盖完整输入的组句候选（列表尾）
    Tail = 3,
    /// 兜底候选（最长前缀回退、声母缩写）
    Fallback = 4,
    /// 首音节单字追加：唯一跨层按有效频率归并的来源，不参与整表排序
    Append = 5,
}

/// 候选来源。用于同文本去重时的优先级打破，以及诊断/测试可观测性。
///
/// 序号即优先级（小者优先）：词典 > 用户词 > 缩写 > 英文 > 组句 > 首字 > 兜底 > AI。
/// 用户词排在词典之后是因为它与词典同层竞争时只改频率、不改层归属；AI 垫底是
/// 因为它不参与主排序，只做末尾追加。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceKind {
    /// 中文词典条目
    Dict = 0,
    /// 用户词 overlay（学到过的词，与词典同层合并）
    User = 1,
    /// 声母缩写命中
    Abbrev = 2,
    /// 英文词典条目
    English = 3,
    /// 组句候选（Viterbi / 整句）
    Sentence = 4,
    /// 首音节单字追加
    FirstSyllable = 5,
    /// 兜底（最长前缀回退）
    Fallback = 6,
    /// AI 预测候选
    Ai = 7,
}

/// 候选词 —— 全链路统一货币：dict 查询产出、rank 排序、engine 翻页、AI 层追加。
///
/// 不变量（由 [`crate::rank`] 与 dict/engine 构造点共同保证）：
///
/// - `freq` 恒为库内原始值，`eff` 恒为排序用有效频率（含用户学习加成）；
/// - `lang` 与 `pinyin` 的解释一致：英文候选的 `pinyin` 是词本身；
/// - `layer` 与候选的产生阶段一致：补全候选不得标 [`Layer::Exact`]。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// 显示文本；同时是去重键与同分 tie-break（升序）。
    pub text: String,
    /// 读音键：中文为音节 `'` 连接（`ni'hao`），英文为词本身。解释权归 `lang`。
    pub pinyin: String,
    /// 库内原始频率（导出/持久化契约，永不含学习加成）。
    pub freq: u64,
    /// 排序用有效频率 = 语料裸频 + 用户学习加成（开库/学习时烘焙，热路径只读）。
    pub eff: u64,
    /// true = 来自 AI 预测（UI 标注用）；等价于 `source == SourceKind::Ai`。
    pub ai: bool,
    /// 语言归属；跨语言判断必须先看它。
    pub lang: Lang,
    /// 层级；层序即排序序，层内才允许比较 `eff`。
    pub layer: Layer,
    /// 来源；同文本去重与同分 tie-break 用。
    pub source: SourceKind,
}

impl Candidate {
    /// 构造中文词典候选（层由调用方给出，避免默认层掩盖调用点语义）。
    pub fn zh(text: impl Into<String>, pinyin: impl Into<String>, freq: u64, eff: u64) -> Self {
        Self {
            text: text.into(),
            pinyin: pinyin.into(),
            freq,
            eff,
            ai: false,
            lang: Lang::Zh,
            layer: Layer::Exact,
            source: SourceKind::Dict,
        }
    }

    /// 构造英文词典候选（读音键 = 词本身）。
    pub fn en(text: impl Into<String>, freq: u64, eff: u64) -> Self {
        let text = text.into();
        Self {
            pinyin: text.clone(),
            text,
            freq,
            eff,
            ai: false,
            lang: Lang::En,
            layer: Layer::Exact,
            source: SourceKind::English,
        }
    }

    /// 覆盖层级（构造后定型，保证「层由产生阶段决定」在类型可读的地方只有一处）。
    pub fn with_layer(mut self, layer: Layer) -> Self {
        self.layer = layer;
        self
    }

    /// 覆盖来源。
    pub fn with_source(mut self, source: SourceKind) -> Self {
        self.ai = source == SourceKind::Ai;
        self.source = source;
        self
    }

    /// 排序键的比较入口由 [`crate::rank`] 独占实现；此处仅暴露字段供其构造。
    pub fn rank_fields(&self) -> (Lang, Layer, u64, &str, SourceKind) {
        (
            self.lang,
            self.layer,
            self.eff,
            self.text.as_str(),
            self.source,
        )
    }
}

/// 排序键：`(tier, score DESC, text ASC, source ASC)`。
///
/// 这是**唯一**的排序语义定义处。三个要素各管一件事：
///
/// - `tier`（层）：匹配质量。精确 > 覆盖句 > 补全 > 尾部 > 兜底 > 追加。
///   层在分数之前 ⇒ 「精确命中整体在补全之前，哪怕补全频率高一个数量级」是结构性事实，
///   任何频率、任何加成都跨不过去。
/// - `score`（校准分数）：同一层内的频率序。中文与英文各自在自己的分布里归一化后
///   才可比（见 [`crate::rank::ScoreScale`]）。
/// - `text` / `source`：确定性 tie-break。
///
/// # 为什么语言**不是**排序维度
///
/// 曾经把 `lang` 放在排序键最高位（=「中文整块在前」），也曾试过「同语言比层、跨语言
/// 只比分」。后者看起来更「纯按分数」，但它**不是全序**：取
/// `a=(Zh,Exact,10)`、`b=(Zh,Completion,100)`、`c=(En,Exact,50)`，有
/// `a<b`（层）、`b<c`（分数）、`c<a`（分数），构成 `a<b<c<a` 环。
/// 比较器一旦成环，`sort_by` 的结果就不再由输入唯一决定（对同一个多重集，换个初始
/// 顺序就换个输出），跨语言名次不可复现。
///
/// 根本原因是：要求「精确恒在补全之前」+「跨语言只比分」+「比较器可传递」三条同时
/// 成立是不可能的——只要层序是全局的，分数就永远越不过层；只要跨语言真只比分，
/// 层序就必须是局部的，而局部层序无法与全局分数拼成可传递的比较。
///
/// 取舍：保留「层完整性」这条**用户可感知**的契约（精确命中压住补全，是所有中文输入
/// 法的共识），让跨语言竞争发生在**同层之内**——同为精确层时中文与英文按校准分数
/// 交错，同为补全层时同理。于是「英文精确词压过中文补全词」仍然会发生（那是层序），
/// 但「英文补全词 `rug` 压过中文补全词「如果」」不会——后者正是本次要修的 bug。
///
/// 借用字段而非拥有 `String`，避免每次比较发生分配（热路径每键一次排序，数十条）。
#[derive(Clone, Copy)]
pub struct RankKey<'a> {
    pub tier: Layer,
    /// 校准后的可比分数（降序比较）
    pub score: u64,
    /// 同分文本升序（确定性）
    pub text: &'a str,
    pub source: SourceKind,
}

impl<'a> RankKey<'a> {
    pub fn of(c: &'a Candidate, score: u64) -> Self {
        Self {
            tier: c.layer,
            score,
            text: c.text.as_str(),
            source: c.source,
        }
    }
}

impl Ord for RankKey<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.tier
            .cmp(&other.tier)
            // 层内：分数降序
            .then_with(|| other.score.cmp(&self.score))
            // 同分：文本升序（与输入顺序无关，保证结果可复现）
            .then_with(|| self.text.cmp(other.text))
            // 同文本同分：来源优先级
            .then_with(|| self.source.cmp(&other.source))
    }
}

impl PartialOrd for RankKey<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for RankKey<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for RankKey<'_> {}

/// 读音键前缀查询的**排他上界**：末字符 +1（`'z'` → `'{'`），`'` 结尾 → `'a'`。
///
/// 为什么放在模型层：读音键（`ni'hao`）的字典序区间是「补全层」的定义基础，两条
/// 存储路径（内存有序索引 / 只读块存储）必须用**同一个**上界函数，否则同一输入在
/// 两条路径上会切出不同的补全区间。历史实现各有一份副本，且曾因对 `'z'` 返回
/// `None` 导致 SQLite 路径把整个补全区间塌缩成空集——这类分歧正是本函数只保留
/// 一份实现的原因。
///
/// 空串返回 `None`（无意义的上界）；调用方要么在入口挡掉空 joined，要么显式决定
/// 退化行为。
pub fn prefix_upper_bound(prefix: &str) -> Option<String> {
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
