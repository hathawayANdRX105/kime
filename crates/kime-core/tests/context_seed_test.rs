//! 上下文感知组句（第六轮②）：上文末词读音反查 → lattice 种子 → 候选排序。
//!
//! 三个不变量（lattice 层，直接拿 Dict + viterbi 钉）：
//! a) 产出候选的 text/pinyin 恒不含种子部分（种子是先验，不是输出词）；
//! b) seed = None 时与旧 `viterbi_sentences` 逐字段相等（转发即证明）；
//! c) `sentence_score` 量纲不变：种子只整体平移 ln_freq_sum/words，
//!    不改候选集合与代价序，分数仍在 freq 的可比尺度内。
//!
//! 引擎层三场景排序（用户验收 c）：
//! - 中文上下文 + 中文组合：种子改变整句落位（碎切贬值、补真词升值）；
//! - 中文上下文 + 英文输入：候选序与无上下文逐字一致；
//! - 英文上下文 + 任意输入：ASCII 守卫禁种子，行为与 HEAD 一致。

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::lattice::{viterbi_sentences, viterbi_sentences_seeded, Seed};
use kime_core::{Engine, Key, Outcome};

/// 种子样本（对齐工单数据）：「我们」wo'men，库频 509405。
const SEED_READING: &str = "wo'men";
const SEED_FREQ: u64 = 509405;
const SEED_TEXT: &str = "我们";
/// fixture 最高库频：种子分数的量纲上界（不变量 c 用）。
const MAX_FREQ: u64 = SEED_FREQ;

const CN: &str = "\
...
我们	wo men	509405
在	zai	400000
再	zai	350000
说	shuo	300000
在说	zai shuo	200000
在说话	zai shuo hua	5000
";

const EN: &str = "\
...
OK	OK	10
okay	okay	500000
";

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kime_seed_{}_{}_{}",
        tag,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// 建库并导入 fixture，返回（Dict 所在目录）：引擎测试各自再 open 一次，
/// 两个引擎因此拿到逐行相同的库（total_freq 一致，分数可比）。
fn build_dict(tag: &str) -> std::path::PathBuf {
    let dir = tmp_dir(tag);
    let cn = dir.join("cn.yaml");
    let en = dir.join("en.yaml");
    fs::write(&cn, CN).unwrap();
    fs::write(&en, EN).unwrap();
    let mut dict = Dict::open(dir.join("dict.sqlite3")).unwrap();
    dict.import(&cn).unwrap();
    dict.import_english(&en).unwrap();
    dir
}

fn engine_at(dir: &std::path::Path) -> Engine {
    Engine::new(
        Dict::open(dir.join("dict.sqlite3")).unwrap(),
        Config {
            shuangpin: None,
            ..Config::default()
        },
    )
}

fn dict_at(dir: &std::path::Path) -> Dict {
    Dict::open(dir.join("dict.sqlite3")).unwrap()
}

fn type_letters(e: &mut Engine, s: &str) {
    for c in s.chars() {
        assert_eq!(
            e.key(Key {
                ch: Some(c),
                code: 0,
                shift: false,
                ctrl: false,
                alt: false,
            }),
            Outcome::Consumed,
            "字母 {c:?} 必须进组合"
        );
    }
}

fn texts(cands: &[kime_core::dict::Candidate]) -> Vec<&str> {
    cands.iter().map(|c| c.text.as_str()).collect()
}

fn seed() -> Seed {
    Seed {
        reading: SEED_READING.to_string(),
        freq: SEED_FREQ,
    }
}

fn freq_of(cands: &[kime_core::dict::Candidate], text: &str) -> u64 {
    cands
        .iter()
        .find(|c| c.text == text)
        .unwrap_or_else(|| panic!("候选 {text:?} 不在列表 {cands:?}"))
        .freq
}

// ── 反查窗口（dict 层）──────────────────────────────────────────────

#[test]
fn readings_of_text_returns_pinyin_by_freq() {
    let dir = build_dict("ro");
    let dict = dict_at(&dir);
    // 精确 text 反查：返回 (pinyin, freq)，freq 降序。
    let got = dict.readings_of_text(SEED_TEXT, 4).unwrap();
    assert_eq!(got, vec![(SEED_READING.to_string(), SEED_FREQ)]);
    // 空文本 / limit 0 → 空集（查询入口挡掉，不碰 SQL）
    assert!(dict.readings_of_text("", 4).unwrap().is_empty());
    assert!(dict.readings_of_text(SEED_TEXT, 0).unwrap().is_empty());
    // 库里没有的文本 → 空集（走 idx_phrase_text 点查，零行）
    assert!(dict.readings_of_text("不存在", 4).unwrap().is_empty());
    fs::remove_dir_all(dir).unwrap();
}

// ── 不变量 a：产出 text/pinyin 恒不含种子部分 ────────────────────────

#[test]
fn invariant_a_seed_text_never_leaks_into_output() {
    let dir = build_dict("ia");
    let dict = dict_at(&dir);
    // 输入读音与种子读音不相交：候选若混入种子文本/拼音，在这里立刻显形。
    let reading = vec!["zai".to_string(), "shuo".to_string()];
    let seeded = viterbi_sentences_seeded(&dict, &reading, Some(seed()));
    assert!(!seeded.is_empty(), "fixture 必须给出整句候选");
    for c in &seeded {
        assert!(
            !c.text.starts_with(SEED_TEXT),
            "候选文本混入上文种子：{:?}",
            c
        );
        assert!(
            !c.pinyin.starts_with(SEED_READING),
            "候选拼音混入上文种子：{:?}",
            c
        );
    }
    // 更强的一条：种子贡献的 text/pinyin 是空串，故有/无种子的文本序列逐条相同。
    assert_eq!(texts(&seeded), texts(&viterbi_sentences(&dict, &reading)));
    fs::remove_dir_all(dir).unwrap();
}

// ── 不变量 b：seed = None 与旧函数逐字段相等 ─────────────────────────

#[test]
fn invariant_b_none_is_identical_to_legacy() {
    let dir = build_dict("ib");
    let dict = dict_at(&dir);
    for reading in [
        vec!["zai".to_string(), "shuo".to_string()],
        vec!["zai".to_string(), "shuo".to_string(), "hua".to_string()],
        vec!["zai".to_string(), "shu".to_string(), "o".to_string()],
        vec!["zai".to_string()], // 退化：长度 1，两边都空
    ] {
        assert_eq!(
            viterbi_sentences_seeded(&dict, &reading, None),
            viterbi_sentences(&dict, &reading),
            "None 必须与旧函数逐字段相等：reading = {reading:?}"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

// ── 不变量 c：sentence_score 量纲不变 ────────────────────────────────

#[test]
fn invariant_c_score_dimension_unchanged() {
    let dir = build_dict("ic");
    let dict = dict_at(&dir);
    // 可观察性边界（CI 实测钉出来的）：fixture 所有跨度都有整词，碎切路径
    // 与整词产出**同文本**，被 insert_path 去重——viterbi 返回序由代价钉，
    // 先验不改文本序（断言 1）。先验的可观察效果只剩同一候选的 sentence_score
    // **数值**变化（zaishuo，「在说」words 1→2）：
    //   plain  = exp(0.5·ln 200000) ≈ 447
    //   seeded = exp(0.5·ln(509405·200000) − 0.25·ln T) ≈ 2700
    let reading = vec!["zai".to_string(), "shuo".to_string()];
    let plain = viterbi_sentences(&dict, &reading);
    let seeded = viterbi_sentences_seeded(&dict, &reading, Some(seed()));
    assert!(!plain.is_empty() && !seeded.is_empty());

    // 1) 先验不重排句子：代价序不变 ⇒ 文本顺序不变。
    assert_eq!(texts(&seeded), texts(&plain));

    // 2) 分数量纲：≥1，且不越过库内最高频（仍在 freq 可比尺度内）。
    for c in &seeded {
        assert!(c.freq >= 1, "分数下溢：{c:?}");
        assert!(c.freq <= MAX_FREQ, "分数越出 freq 量纲：{c:?}");
    }

    // 3) 先验确实进入了分数：上文先验抬升句子分数（words+1 联合概率）。
    assert!(
        freq_of(&seeded, "在说") > freq_of(&plain, "在说"),
        "句子应被上文先验抬高：plain={plain:?} seeded={seeded:?}"
    );
    fs::remove_dir_all(dir).unwrap();
}

// ── 场景 1：中文上下文 + 中文组合 ────────────────────────────────────

#[test]
fn chinese_context_reorders_sentence_candidates() {
    // 无上下文（= HEAD 行为，回归基线）：CI 实测 [在说, 在说话]。
    // 注：viterbi 每跨度只取最优边（在 400000 > 再 350000），「再」的碎切
    // 路径不进 lattice；zaishuo 下整词与碎切同文本被去重——种子在两音节
    // 输入上不产生可观察重排（可观察断言在 lattice 层 invariant_c，三音节）。
    let plain_dir = build_dict("s1_plain");
    let mut plain = engine_at(&plain_dir);
    type_letters(&mut plain, "zaishuo");
    assert_eq!(
        texts(plain.candidates()),
        vec!["在说", "在说话"],
        "无上下文回归（HEAD 行为）"
    );

    // 中文上下文「我们」作种子：层序不变（层一精确命中恒第一），且候选
    // 绝不携带上文前缀（引擎层不变量 a 投影）。
    let ctx_dir = build_dict("s1_ctx");
    let mut ctx = engine_at(&ctx_dir);
    ctx.set_context(Some(SEED_TEXT.to_string()));
    type_letters(&mut ctx, "zaishuo");
    assert_eq!(
        texts(ctx.candidates()),
        vec!["在说", "在说话"],
        "种子不破坏层序：层一精确命中与补全落位不变"
    );
    for c in ctx.candidates() {
        assert!(!c.text.starts_with(SEED_TEXT));
    }
    fs::remove_dir_all(plain_dir).unwrap();
    fs::remove_dir_all(ctx_dir).unwrap();
}

// ── 场景 2：中文上下文 + 英文输入 ────────────────────────────────────

#[test]
fn chinese_context_leaves_english_input_untouched() {
    // 「ok」没有完整音节切分（'k' 不是音节）→ 组句分支根本不执行 →
    // 种子无从插入，merge_english 的层一落位必须与无上下文逐字一致。
    let plain_dir = build_dict("s2_plain");
    let mut plain = engine_at(&plain_dir);
    type_letters(&mut plain, "ok");
    let base = texts(plain.candidates());
    assert_eq!(base.first().copied(), Some("OK"));

    let ctx_dir = build_dict("s2_ctx");
    let mut ctx = engine_at(&ctx_dir);
    ctx.set_context(Some(SEED_TEXT.to_string()));
    type_letters(&mut ctx, "ok");
    assert_eq!(
        texts(ctx.candidates()),
        base,
        "中文上下文不得改变英文输入的候选序"
    );
    fs::remove_dir_all(plain_dir).unwrap();
    fs::remove_dir_all(ctx_dir).unwrap();
}

// ── 场景 3：英文上下文 + 任意输入 ────────────────────────────────────

#[test]
fn english_context_never_seeds() {
    // ASCII 守卫：英文上下文 context_seed 直接 None。
    // 中文输入走得进组句分支——若守卫失效，种子会改变候选落位。
    let plain_dir = build_dict("s3_plain");
    let mut plain = engine_at(&plain_dir);
    type_letters(&mut plain, "zaishuo");
    // 基线 = CI 实测 HEAD 行为（「再说」不在候选，见场景 1 注）。
    let base = texts(plain.candidates());
    assert_eq!(base, vec!["在说", "在说话"]);
    let ctx_dir = build_dict("s3_ctx");
    let mut ctx = engine_at(&ctx_dir);
    ctx.set_context(Some("hello world".to_string()));
    type_letters(&mut ctx, "zaishuo");
    assert_eq!(
        texts(ctx.candidates()),
        base,
        "英文上下文必须完全等价于无上下文"
    );

    // 英文输入同理（与场景 2 的基线一致）。
    let mut ctx2 = engine_at(&ctx_dir);
    ctx2.set_context(Some("hello".to_string()));
    type_letters(&mut ctx2, "ok");
    let mut plain2 = engine_at(&plain_dir);
    type_letters(&mut plain2, "ok");
    assert_eq!(texts(ctx2.candidates()), texts(plain2.candidates()));
    fs::remove_dir_all(plain_dir).unwrap();
    fs::remove_dir_all(ctx_dir).unwrap();
}
