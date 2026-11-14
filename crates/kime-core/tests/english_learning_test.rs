//! 英文词的学习与排序：用户用过的英文词往上走，用得少的往下沉。
//!
//! 契约（任务书 2026-11-14 的 A1..A5）：
//! - A1：`lookup_english` 的 `eff` = 裸频 + 用户提频加成（与中文路径同一算式）。
//! - A2：英文词上屏 N 次 → `user_stats` 计数 +N，且 `eff` 相比未使用时上升。
//! - A3：常用词（`ruin`）用过之后排在低频词（`rug`/`gob`）之前。
//! - A4：不在 `english` 表里的串（`zxqv`）不产生任何学习记录，候选集合不变。
//! - A5：分层契约不被提频打破——层二词（`ifx`）不得插到层一词（`if`）之前。
//!
//! 中文候选的排序与学习零改动：这里同时钉「英文计数不进 `user_pinyins`」。

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::dict::{Candidate, Dict};
use kime_core::{Engine, Key, Outcome};

/// 中文表：与英文同形的 `ni`/`he` 用来验「英文计数不进中文提频」。
const CN: &str = "\
...
你好\tni hao\t9000000
拟好\tni hao\t4000000
你\tni\t6000000
喝\the\t500
河\the\t400
";

/// 英文表：`if`/`ruin` 低频，`rug`/`gob` 高频，`ifx` 是 `if` 的补全（层二）。
///
/// 频率刻意拉开：`ruin`(100) 低于 `rug`(900)：不学习时 `rug` 在前，
/// 学习 `ruin` 之后 `ruin` 必须反超——这正是用户诉求「我常用的要靠前」。
/// 同前缀的词才落在同一个候选集里可比：`rug`/`ruin` 共前缀 `ru`（A3），
/// `if`/`ifx` 共前缀 `if`（A5 的层一/层二契约）。
const EN: &str = "\
...
if\tif\t100
ruin\truin\t100
rug\trug\t900
gob\tgob\t800
ifx\tifx\t5
ni\tni\t300
";

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kime_enlearn_{}_{}_{}",
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

/// 建库：导中文 + 英文，返回 (Dict, 目录)。
fn seeded(tag: &str) -> (Dict, std::path::PathBuf) {
    let dir = tmp_dir(tag);
    let cn = dir.join("cn.yaml");
    let en = dir.join("en.yaml");
    fs::write(&cn, CN).unwrap();
    fs::write(&en, EN).unwrap();
    let mut dict = Dict::open(dir.join("dict.sqlite3")).unwrap();
    dict.import(&cn).unwrap();
    dict.import_english(&en).unwrap();
    (dict, dir)
}

fn texts(hits: &[Candidate]) -> Vec<String> {
    hits.iter().map(|c| c.text.clone()).collect()
}

/// 收尾：先 drop 掉 Dict（`LearnWriter::drop` 会 join 后台写线程、排空队列），
/// 再删目录——顺序反了会撞上后台线程重建的 `-wal`/`-shm` 文件（Directory not empty）。
fn cleanup(dict: Dict, dir: std::path::PathBuf) {
    drop(dict);
    let _ = fs::remove_dir_all(dir);
}

fn eff_of(hits: &[Candidate], text: &str) -> u64 {
    hits.iter()
        .find(|c| c.text == text)
        .map(|c| c.eff)
        .unwrap_or_else(|| panic!("候选里没有 {text}"))
}

/// A1：`eff` = 裸频 + 用户提频加成（首用即满额 = +300k，与中文路径同一算式）。
#[test]
fn english_eff_is_freq_plus_user_bonus() {
    let (mut d, dir) = seeded("a1");
    let before = d.lookup_english("if", 10);
    assert_eq!(eff_of(&before, "if"), 100, "未学习时 eff = 裸频");

    assert!(d.learn_english("if"), "词库里的英文词必须能学");
    let after = d.lookup_english("if", 10);
    assert_eq!(
        eff_of(&after, "if"),
        100 + 300_000,
        "首用即满额：eff = freq + USER_BOOST"
    );
    assert_eq!(after[0].freq, 100, "freq 恒为词库原值：加成只落 eff");
    cleanup(d, dir);
}

/// A2：同一词上屏 N 次 → 计数 +N，eff 随之上升。
#[test]
fn repeated_use_raises_count_and_eff() {
    let (mut d, dir) = seeded("a2");
    assert_eq!(d.english_usage("if"), None, "没学过就没有记录");
    for _ in 0..3 {
        assert!(d.learn_english("if"));
    }
    assert_eq!(d.english_usage("if"), Some(3), "计数应累加到 3");
    let hits = d.lookup_english("if", 10);
    assert_eq!(
        eff_of(&hits, "if"),
        100 + 3 * 300_000,
        "eff 应随使用次数上升"
    );
    cleanup(d, dir);
}

/// A3：用户诉求的直接验收——常用的 `ruin` 用过之后排在没用过的 `rug` 之前。
///
/// `rug`(900) 与 `ruin`(100) 同前缀 `ru`，才落在同一个候选集里可比；
/// `if`/`ifx` 同前缀 `if`，供 A5 的分层契约用。
#[test]
fn frequently_used_word_overtakes_unused_words() {
    let (mut d, dir) = seeded("a3");
    let before = texts(&d.lookup_english("ru", 10));
    assert_eq!(before, ["rug", "ruin"], "前置条件：未学习时高频的 rug 在前");

    d.learn_english("ruin");
    let after = d.lookup_english("ru", 10);
    assert_eq!(
        texts(&after),
        ["ruin", "rug"],
        "常用的 ruin 应排到没用过的 rug 之前"
    );
    assert_eq!(
        eff_of(&after, "ruin"),
        100 + 300_000,
        "ruin 的 eff 应含首用满额加成"
    );
    assert_eq!(eff_of(&after, "rug"), 900, "没用过的 rug 只有裸频");
    cleanup(d, dir);
}

/// A4：不在 `english` 表里的串不产生任何学习记录，候选集合不变。
#[test]
fn unknown_word_is_not_learned() {
    let (mut d, dir) = seeded("a4");
    let before = texts(&d.lookup_english("if", 10));
    assert!(!d.learn_english("zxqv"), "词库外的词不得学习");
    assert_eq!(d.english_usage("zxqv"), None, "词库外的词不得进计数");
    let after = texts(&d.lookup_english("if", 10));
    assert_eq!(before, after, "候选集合不得变化");
    cleanup(d, dir);
}

/// A5：分层契约不被提频打破——层二词（`ifx`）不得插到层一词（`if`）之前。
#[test]
fn boosted_completion_never_jumps_ahead_of_exact_hit() {
    let (mut d, dir) = seeded("a5");
    // `ifx` 用得再多，层一（`if`）仍整体在前。
    for _ in 0..5 {
        d.learn_english("ifx");
    }
    let hits = d.lookup_english("if", 10);
    let texts = texts(&hits);
    let if_pos = hits.iter().position(|c| c.text == "if").unwrap();
    let ifx_pos = hits.iter().position(|c| c.text == "ifx").unwrap();
    assert!(
        if_pos < ifx_pos,
        "层一词 if 必须整体在层二词 ifx 之前：{texts:?}"
    );
    cleanup(d, dir);
}

/// 英文计数不得污染中文提频：`user_pinyins` 只认中文键，
/// 英文词（两段相等）不进这个集合，中文候选顺序零改动。
#[test]
fn english_learning_does_not_disturb_chinese_candidates() {
    let (mut d, dir) = seeded("cn");
    let before = texts(&d.lookup_prefix(&["ni".to_string()], "", 10).unwrap());
    // `ni` 在英文表里也存在：这一学必须真的写进英文计数，才谈得上「不污染中文」。
    assert!(d.learn_english("ni"), "ni 在英文表里，必须能学");
    assert_eq!(d.english_usage("ni"), Some(1), "英文计数已写入");
    let after = texts(&d.lookup_prefix(&["ni".to_string()], "", 10).unwrap());
    assert_eq!(
        before, after,
        "英文计数不得改变中文候选顺序：{before:?} → {after:?}"
    );
    cleanup(d, dir);
}

/// A6：不回归——英文候选的分层、单字母闸、大小写不敏感匹配、`AA`/`aa` 独立条目
/// 全部保持原样（学习只改层内次序，不改命中集合与分层）。
#[test]
fn english_lookup_contract_is_unchanged_by_learning() {
    let (mut d, dir) = seeded("a6");
    // 学习前：层一（`if` 精确命中）整体在层二（`ifx` 补全）之前。
    assert_eq!(
        texts(&d.lookup_english("if", 10)),
        ["if", "ifx"],
        "层一整体在层二之前"
    );
    // 单字母闸在引擎侧（`merge_english` 的 letters.len() < 2），不在 `lookup_english`：
    // 这里只钉字典层契约——`i` 作为前缀照样能查到补全（层二），与改动前一致。
    assert_eq!(
        texts(&d.lookup_english("i", 10)),
        ["if", "ifx"],
        "前缀查询返回层二补全（单字母闸在引擎侧）"
    );
    // limit 截断：层一名额优先。
    assert_eq!(
        texts(&d.lookup_english("if", 1)),
        ["if"],
        "limit=1 时层一占满，补全一个都拿不到"
    );

    d.learn_english("ifx");
    // 学习后：命中集合与分层不变，只有层内次序可能变。
    let hits = d.lookup_english("if", 10);
    assert_eq!(texts(&hits), ["if", "ifx"], "学习不得改变命中集合与分层");
    cleanup(d, dir);
}

/// `AA` 与 `aa` 是两条独立条目：学习 `aa` 只加 `aa` 的加成，`AA` 不受影响。
#[test]
fn case_variants_are_learned_independently() {
    let dir = tmp_dir("case");
    let en = dir.join("en.yaml");
    fs::write(
        &en,
        "\
...
AA\tAA\t50
aa\taa\t20
",
    )
    .unwrap();
    let mut d = Dict::open(dir.join("dict.sqlite3")).unwrap();
    d.import_english(&en).unwrap();

    assert!(d.learn_english("aa"), "aa 必须能学");
    let hits = d.lookup_english("aa", 10);
    assert_eq!(eff_of(&hits, "aa"), 20 + 300_000, "aa 应拿到自己的加成");
    assert_eq!(
        eff_of(&hits, "AA"),
        50,
        "AA 是独立条目，不该被 aa 的使用次数牵连"
    );
    cleanup(d, dir);
}

/// 英文词**不进** `phrase` 表（那张表是拼音前缀查询的数据源）。
#[test]
fn english_learning_never_writes_the_phrase_table() {
    let (mut d, dir) = seeded("phrase");
    d.learn_english("if");
    let rows: i64 = d
        .conn()
        .query_row(
            "SELECT count(*) FROM phrase WHERE text LIKE '%if%' AND user = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0, "英文词不得进 phrase 表");
    cleanup(d, dir);
}

/// A7（不吞键）：英文模式下每个字母键的可见行为与改动前逐字一致——
/// 仍是 `Outcome::Ignored`（壳层据此转发给应用），首字母与后续字母一样。
#[test]
fn english_mode_letters_are_still_ignored() {
    let (dict, dir) = seeded("a7");
    let db_path = dir.join("dict.sqlite3");
    let mut e = Engine::new(dict, Config::default());
    e.set_chinese(false);
    for c in "if".chars() {
        assert_eq!(
            e.key(Key {
                ch: Some(c),
                code: 0,
                shift: false,
                ctrl: false,
                alt: false,
            }),
            Outcome::Ignored,
            "英文模式的字母键必须仍然放行（不得被吞）"
        );
    }
    drop(e);
    cleanup(Dict::open(&db_path).unwrap(), dir);
}

/// 英文模式下打字 → 空格分词 → 词被学习（学习闭环的端到端路径）。
#[test]
fn english_mode_typing_then_space_learns_the_word() {
    let (dict, dir) = seeded("e2e");
    let db_path = dir.join("dict.sqlite3");
    let mut e = Engine::new(dict, Config::default());
    e.set_chinese(false);
    for c in "if".chars() {
        e.key(Key {
            ch: Some(c),
            code: 0,
            shift: false,
            ctrl: false,
            alt: false,
        });
    }
    // 空格是分词边界：词在此结算并学习。
    e.key(Key {
        ch: None,
        code: 57,
        shift: false,
        ctrl: false,
        alt: false,
    });
    assert_eq!(
        e.english_usage("if"),
        Some(1),
        "空格边界应把累积的英文词学进去"
    );
    drop(e);
    cleanup(Dict::open(&db_path).unwrap(), dir);
}
