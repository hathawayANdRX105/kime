//! Word Lattice + Viterbi 句级联想
//!
//! 给定一段由 `segment` 产生的完整音节切分 `reading: &[String]`（长度 > 1），
//! 构建 Word Lattice 并在其上跑 Viterbi 最短路径，输出最优组合词序列。
//! 每格保留多个候选词、各建成一条边（[`SPAN_CANDIDATES`]），每节点保留
//! [`PATHS_PER_NODE`] 条切分；某音节词库无任何条目时插占位边
//! （[`UNKNOWN_LOG_PROB`]）保证路径总能走通。
//! [`viterbi_sentences_seeded`] 额外把光标前已上屏的末词（[`Seed`]）当作虚拟起点词，
//! 其先验概率计入联合概率，但不进产出候选的文本（只上屏新组合）。

use crate::dict::{Candidate, Dict};

/// 每条边的基础代价。每个词都收一份，构成「段数少优先」的固定偏置；
/// 概率项本身（每个多出来的词平均再加 ~10000）已经保证了不会乱切。
const BASE_COST: f64 = 10000.0;
/// 概率权重：-ln(p) 的放大系数，决定同样切分数下对高频词的偏好强度。
const FREQ_WEIGHT: f64 = 1000.0;
/// 每个词的额外惩罚，抑制把长串切成一堆单字。
const WORD_PENALTY: f64 = 50.0;

/// 每格（跨度）候选上限：≤ 4 音节的跨度最多保留 6 个同音词、全部建成边
/// （qingjian SPAN_CANDIDATES=6 同款思路）：1–2 音节跨度正是同音词密集区
/// （shi：时/事/市/实/识……），只留 1 个会把每种同音切分都压成单选；3–4 音节
/// 同音密度已低，6 个是留足余量。该值同时是传给 `dict.lookup` 的 limit。
const SPAN_CANDIDATES: usize = 6;

/// 长跨度阈值：超过 4 音节的跨度只保留最高频 1 个候选——那么长的词已是
/// 成语级、极少，且同音率趋零，多留纯浪费；limit=1 直传 lookup 也省
/// 无用扫描。取 4 而非 qingjian 的 MAX_WORD_SYLLABLES=8：本词库基本没有
/// 超过 4 音节的词，更长跨度只值得一次 LIMIT-1 查词。
const MAX_SPAN_SYLLABLES: usize = 4;

/// 词库查无此音节时的兜底 log 概率（qingjian UNKNOWN_LOG_PROB 同款思路）：
/// 节点所有跨度都查不到词时，用音节本身补一条占位边，让路径恒能走通；
/// -30 比词库内任何可命中词的最低概率低得多，占位句在候选列里恒垫底。
const UNKNOWN_LOG_PROB: f64 = -30.0;

/// 路径的候选分数：把词频乘积折算成与单词 `freq` 同量纲、可同列排序的数值。
///
/// `score = √(∏ fᵢ) / T^((k-1)/4)`，再对 k≥3 的碎片路径每个额外词 ×1/100。
/// 两端都被实测否证过：旧的算术平均（`(Σfᵢ)/k`，千万级）让任何两个高频字的
/// 碎句压过一切真词；纯联合概率（`∏fᵢ/T^(k-1)`，几百度量级）又把所有组合
/// 埋到最生僻的补全词之下（`我们去` 486 < `我们确信` 2035 → 回到工单第 4 条的
/// 病态）。开方阻尼取几何平均与联合概率之间：实测 `我们去`≈6.5k 排进
/// `我们确信`(2k) 之前、`今天完`≈3k 让位给真词 `今天晚上`(51k)。
/// 二次修正（÷100 每多一个词）：词频高的单字把 k≥3 的分数抬进噪声区
/// （`最进好` 82k 反而压过真补全 `最近好吗`），而 3+ 词组合几乎全是碎切，
/// 打折把它们沉回补全之后。k 更大的长句（`我不知道你说的是什么`）分数
/// 落到地板也无妨——那类输入通常根本没有竞争候选。
fn sentence_score(ln_freq_sum: f64, words: usize, total: f64) -> u64 {
    let extra = words.saturating_sub(2) as f64;
    let score =
        (0.5 * ln_freq_sum - 0.25 * (words - 1) as f64 * total.ln() - 4.60517 * extra).exp();
    score.floor().max(1.0) as u64
}

/// 一条到某个音节节点的部分路径（k-best Viterbi 的 DP 状态）。
struct Path {
    cost: f64,
    text: String,
    pinyin: String,
    ln_freq_sum: f64,
    words: usize,
    /// 路径是否含占位边（词库无该音节的条目，用音节字母顶替）。
    /// 占位句的 text 含拼音字母，不是可上屏的中文，故不作为候选输出
    /// （qingjian 的 `has_placeholder` 同款门禁）；占位边只在路径内传递，
    /// 保证「有未收录音节时句子仍能拼出来供诊断」，不进 `Candidate`。
    placeholder: bool,
}

/// 每个音节节点保留的最优路径数（k-best 束宽，qingjian BEAM_WIDTH=8 同款思路）。
/// 3 曾被实测剪掉：`womenzai` 的「我们+再」整条路径（词库里存在整句词条时
/// 其路径代价恒排第三）被整条剪掉，4 音节场景下 2+2 切分进不了候选列；
/// 现在每格多候选词（[`SPAN_CANDIDATES`]）后前缀分歧更大，束宽 8 才装得下
/// 「节点最优 + 备选切分 + 高频组合」。垃圾路径不会跟着变多：k≥3 的分数
/// 被 `sentence_score` 的 ÷100/词 压死，且 place_sentences 按分数在补全区落位。
const PATHS_PER_NODE: usize = 8;

/// 上文末词种子（上下文感知组句）：把光标前已上屏的最后一个词当作**虚拟起点词**。
///
/// - `reading` = 该词拼音（`'` 连接，如 `"wo'men"`），`freq` = 库内裸频
///   （由 [`crate::dict::Dict::readings_of_text`] 反查得到）。
/// - 种子贡献的是**先验概率**，不是输出词：其 `-ln(p)` 进路径代价、`ln(freq)` 进
///   `ln_freq_sum`、`words` 计 +1，但产出候选的 `text`/`pinyin` **恒不含种子部分**
///   （只上屏新组合，见 [`viterbi_sentences_seeded`] 的不变量 a）。
/// - `reading` 因此不参与构造候选 pinyin，只用于诊断/可追溯：引擎据此知道种子来自
///   哪一路反查命中，`None` = 无上下文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seed {
    pub reading: String,
    pub freq: u64,
}

/// [`viterbi_sentences_seeded`] 的无种子转发（不变量 b：同输入同输出，含 PATHS_PER_NODE）。
pub fn viterbi_sentences(dict: &Dict, reading: &[String]) -> Vec<Candidate> {
    viterbi_sentences_seeded(dict, reading, None, &mut SpanCache::default())
}

/// 词图格子候选缓存：键 = 跨度的 `'` 连接拼音串，值 = 排好序的 `lookup` 结果。
///
/// 敲键是增量的：第 n+1 键的词图只**新增**以它结尾的几个跨度，其余跨度上一键
/// 都查过——而格子查词是整句转换最贵的一步（qingjian 同款结论，他们借此
/// 12ms→1ms）。缓存失效由持有方负责：凡词库内容或条目 eff 可能变（learn /
/// import），整个 clear。超 [`MAX_SPAN_CACHE_ENTRIES`] 上限整体丢弃重建。
#[derive(Default)]
pub struct SpanCache {
    entries: std::collections::HashMap<String, std::sync::Arc<[Candidate]>>,
}

/// 缓存容量上限：一次整句的跨度数 = n(n+1)/2，几十音节的句子几百个，8192 足够；
/// 超过说明失效逻辑漏了，宁可整清不可无限增长。
pub const MAX_SPAN_CACHE_ENTRIES: usize = 8192;

impl SpanCache {
    /// 取缓存跨度候选；没有就 `compute` 一次并存入。
    pub fn get_or_compute(
        &mut self,
        key: String,
        compute: impl FnOnce() -> Vec<Candidate>,
    ) -> std::sync::Arc<[Candidate]> {
        if let Some(found) = self.entries.get(&key) {
            return std::sync::Arc::clone(found);
        }
        if self.entries.len() >= MAX_SPAN_CACHE_ENTRIES {
            self.entries.clear();
        }
        let words: std::sync::Arc<[Candidate]> = compute().into();
        self.entries.insert(key, std::sync::Arc::clone(&words));
        words
    }

    /// 词库内容或条目 eff 变了（learn / import）：全部作废。
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// 当前缓存跨度数（测试/诊断用）。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 缓存是否为空。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Viterbi 最短路径求解，返回最多 [`PATHS_PER_NODE`] 条文本互异的整句候选（代价升序）。
///
/// - `reading`: 由 `segment` 产生的完整音节切分，长度需 ≥ 2
/// - `seed`: 光标前已上屏的末词（[`Seed`]）。`Some` 时它占据虚拟起点 `dp[0]`，
///   代价/概率/词数都算它的，但 `text`/`pinyin` 留空——产出的候选只含本次输入的新组合。
/// - 代价函数：`cost = BASE_COST - ln(p) * FREQ_WEIGHT + WORD_PENALTY`，
///   其中 `p = freq / 语料总词频`。**必须是概率而不是计数**：多词路径的概率是相乘的，
///   用计数比就等于拿 `f(知)×f(道)` 压 `f(知道)`，切得越碎越占便宜（实测「知道」501,255
///   永远输给「知+道」4.4e6×1.06e6，整句吐出「只到」这类错字）。种子与普通边同式计价。
/// - 词数惩罚：每个词 +50，鼓励合词而非全碎成单字
/// - 每格多候选词：一个跨度命中的每个候选词都建一条边（短跨度最多
///   [`SPAN_CANDIDATES`] 个，长跨度只留 1 个）；节点所有跨度都查不到词时
///   插一条单音节占位边（代价按 [`UNKNOWN_LOG_PROB`]）——缺音节组合不再
///   整句落空，占位句恒排候选列末尾。
/// - 候选 `freq` 取 [`sentence_score`]，让整句在层一里按联合概率参与排序。
///
/// 三个不变量（`tests/context_seed_test.rs` 逐条钉住）：
/// a) 产出 `text`/`pinyin` 恒不含种子部分（种子是先验，不进上屏文本）；
/// b) `seed = None` 时与 [`viterbi_sentences`] 完全一致（转发即证明）；
/// c) `sentence_score` 量纲不变：种子只把 `ln_freq_sum`/`words` 整体平移，
///    不改变候选集合与代价序——种子让长联合（`words` 更大）按 `T` 指数与
///    ÷100/词 惩罚相对贬值，短补真词相对升值，量纲仍在 `freq` 的可比尺度内。
pub fn viterbi_sentences_seeded(
    dict: &Dict,
    reading: &[String],
    seed: Option<Seed>,
    cache: &mut SpanCache,
) -> Vec<Candidate> {
    if reading.len() < 2 {
        return Vec::new();
    }

    let n = reading.len();
    // dp[i] = 到节点 i 的最优若干条路径（代价升序、文本互异）
    let mut dp: Vec<Vec<Path>> = (0..=n).map(|_| Vec::new()).collect();
    // 种子 = 虚拟起点词：占据 dp[0]，概率代价照算，text/pinyin 留空（不变量 a）。
    // 无种子时起点与旧实现逐字段相同（不变量 b）。
    let total = dict.total_freq() as f64;
    dp[0].push(start_path(seed, total));
    let mut reported_err = false;

    // 对每个起点 i，尝试所有终点 j (i < j <= n)。边恒向前：节点 i 处理完后
    // 不会再收到新路径，整段直接 take 走，避开 dp[i] 与 dp[j] 的双重可变借用。
    for i in 0..n {
        let prevs = std::mem::take(&mut dp[i]);
        if prevs.is_empty() {
            continue;
        }
        extend_node(
            &prevs,
            dict,
            reading,
            i,
            n,
            total,
            cache,
            &mut reported_err,
            &mut dp,
        );
    }

    let Some(finals) = dp.pop() else {
        return Vec::new();
    };
    finals
        .into_iter()
        // 占位句的 text 混着拼音字母（如「在shuo」），不是可上屏的中文，
        // 不作为候选输出；路径里保留占位只为让跨越未收录音节的组合仍算得出代价。
        .filter(|p| !p.placeholder)
        .map(|p| {
            let score = sentence_score(p.ln_freq_sum, p.words, total);
            Candidate {
                text: p.text,
                pinyin: p.pinyin,
                freq: score,
                eff: score,
                ai: false,
            }
        })
        .collect()
}

/// dp[0] 的虚拟起点路径：种子只贡献先验概率（text/pinyin 留空，不变量 a）；
/// 无种子时起点全零（不变量 b）。
fn start_path(seed: Option<Seed>, total: f64) -> Path {
    let Some(seed) = seed else {
        return Path {
            cost: 0.0,
            text: String::new(),
            pinyin: String::new(),
            ln_freq_sum: 0.0,
            words: 0,
            placeholder: false,
        };
    };
    let eff = seed.freq.max(1) as f64;
    let p = eff / total;
    Path {
        cost: BASE_COST - p.ln() * FREQ_WEIGHT + WORD_PENALTY,
        text: String::new(),
        pinyin: String::new(),
        ln_freq_sum: eff.ln(),
        words: 1,
        placeholder: false,
    }
}

/// 跨度 `[i..j]` 的候选上限：短跨度 [`SPAN_CANDIDATES`]、长跨度 1——
/// 该 limit 直传 `dict.lookup`（长跨度 LIMIT 1，省无用扫描）。
fn span_limit(syllables: usize) -> usize {
    if syllables <= MAX_SPAN_SYLLABLES {
        SPAN_CANDIDATES
    } else {
        1
    }
}

/// 跨度 `[i..j]` 的候选词（经格子缓存）：最多 [`span_limit`] 个，频次降序。
/// 查词失败每次至多报一次错并标 `failed`——词库坏了调用方靠它跳过占位
/// 边：退化为逐词候选，不产占位句。
fn span_candidates(
    dict: &Dict,
    cache: &mut SpanCache,
    reading: &[String],
    i: usize,
    j: usize,
    reported_err: &mut bool,
) -> (std::sync::Arc<[Candidate]>, bool) {
    let slice = &reading[i..j];
    let limit = span_limit(slice.len());
    let mut failed = false;
    let cands = cache.get_or_compute(slice.join("'"), || match dict.lookup(slice, limit) {
        Ok(c) => c,
        Err(e) => {
            // 每次调用最多报一次：词库坏了会让每个跨度都失败，别刷屏
            if !*reported_err {
                *reported_err = true;
                eprintln!("[kime] 警告：整句联想查词失败 ({e})，本次退化为逐词候选");
            }
            failed = true;
            Vec::new()
        }
    });
    (cands, failed)
}

/// 扩展一个节点：每个命中词的跨度建边进下游；所有跨度都查不到词（且
/// 没有查词失败）时插占位边，保证路径能走通。
fn extend_node(
    prevs: &[Path],
    dict: &Dict,
    reading: &[String],
    i: usize,
    n: usize,
    total: f64,
    cache: &mut SpanCache,
    reported_err: &mut bool,
    dp: &mut Vec<Vec<Path>>,
) {
    let mut any_hit = false;
    let mut any_failed = false;
    for j in (i + 1)..=n {
        let (cands, failed) = span_candidates(dict, cache, reading, i, j, reported_err);
        if failed {
            any_failed = true;
            continue;
        }
        if cands.is_empty() {
            continue;
        }
        any_hit = true;
        grow_with_words(prevs, dict, &cands, total, &mut dp[j]);
    }
    if !any_hit && !any_failed {
        grow_with_placeholder(prevs, &reading[i], &mut dp[i + 1]);
    }
}

/// 一个跨度的每个候选词都接到每条前缀路径上（每格多候选词）：
/// 同音切分不再取决于词库单条最高频词。
fn grow_with_words(
    prevs: &[Path],
    dict: &Dict,
    cands: &[Candidate],
    total: f64,
    target: &mut Vec<Path>,
) {
    for word in cands {
        let (edge_cost, ln_freq) = word_edge(dict, word, total);
        for prev in prevs {
            insert_path(
                target,
                append(prev, &word.text, &word.pinyin, edge_cost, ln_freq, false),
            );
        }
    }
}

/// 一个词的单条边代价与 ln(freq)：同式 `BASE_COST - ln(p)*FREQ_WEIGHT +
/// WORD_PENALTY`，p = 有效频率 / 语料总词频。取 dict 的排序用有效频率
/// （用户提频参与组句：刚用过的词把整句抬上去，停用 2 个半衰期后自动落回），
/// 与 lookup 的候选序同一量。
fn word_edge(dict: &Dict, word: &Candidate, total: f64) -> (f64, f64) {
    let eff = dict.effective_freq(word).max(1) as f64;
    let p = eff / total;
    (BASE_COST - p.ln() * FREQ_WEIGHT + WORD_PENALTY, eff.ln())
}

/// 占位边：音节在词库中无条目（连单字都没有），text 用音节本身顶替，
/// 代价按固定未知词 log 概率——比任何真实可命中词都贵得多，占位句
/// 恒排候选列末尾；只保证「能出句」，不抢名次。
fn grow_with_placeholder(prevs: &[Path], syllable: &str, target: &mut Vec<Path>) {
    let edge_cost = BASE_COST - UNKNOWN_LOG_PROB * FREQ_WEIGHT + WORD_PENALTY;
    for prev in prevs {
        insert_path(
            target,
            append(prev, syllable, syllable, edge_cost, UNKNOWN_LOG_PROB, true),
        );
    }
}

/// 把一个词（或占位音节）接到部分路径末尾。种子起点 pinyin 为空串时，
/// 拼接天然丢弃种子部分（不变量 a）。`placeholder` 标记这条边是否占位，
/// 随路径向下传递。
fn append(
    prev: &Path,
    text: &str,
    pinyin: &str,
    edge_cost: f64,
    ln_freq: f64,
    placeholder: bool,
) -> Path {
    Path {
        cost: prev.cost + edge_cost,
        text: format!("{}{}", prev.text, text),
        pinyin: join_pinyin(&prev.pinyin, pinyin),
        ln_freq_sum: prev.ln_freq_sum + ln_freq,
        words: prev.words + 1,
        placeholder: prev.placeholder || placeholder,
    }
}

fn join_pinyin(prev: &str, next: &str) -> String {
    if prev.is_empty() {
        next.to_string()
    } else {
        format!("{}'{}", prev, next)
    }
}

/// 把一条完成路径按代价插入节点列表：同文本只留代价低的，列表长 ≤ [`PATHS_PER_NODE`]。
fn insert_path(list: &mut Vec<Path>, path: Path) {
    let pos = list.partition_point(|p| p.cost < path.cost);
    if list.len() >= PATHS_PER_NODE && pos >= PATHS_PER_NODE {
        return; // 已经容不下更好的
    }
    if list.iter().any(|p| p.text == path.text) {
        return; // 同一文本的更贵切分（不错+报 vs 不+错+报）不值得占名额
    }
    list.insert(pos, path);
    list.truncate(PATHS_PER_NODE);
}
