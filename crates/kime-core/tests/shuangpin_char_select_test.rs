//! 双拼长串逐字单选的**键位消耗**验收测试。
//!
//! 缺陷：选词消耗按候选读音的**拼音字母数**从头切 `letters`，但双拼里 letters
//! 存的是**键位串**、每音节恒 2 键（自然码 neng=`ng` 4 字母只占 2 键）。选「能」
//! （neng 4 字母）会把 4 个键位切掉——`ngld`（能量）直接整串清空，`ngnihk`
//! （能+你+好）切到 `hk` 丢掉中间的 ni。选完一个就接不上下一个字。
//!
//! 修复：`sp_active` 标记当前键串按双拼解释（键位语义），消耗改按 2 键/音节。
//! 全拼路径（含双拼解码失败回退全拼）行为不变，钉在既有全拼测试里。

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::engine::{Engine, Key, Outcome};
use kime_shuangpin::Scheme;

/// 自然码键位：neng=`ng` liang=`ld` gou=`gb` ni=`ni` hao=`hk`。
/// 真实双音节词（能量 / 能够 / 你好）保证首查层一就命中，候选序列确定，
/// 不依赖整句 Viterbi 的枚举顺序。
const SEED: &str = "\
...
能量\tneng liang\t900000
能够\tneng gou\t600000
能\tneng\t800000
呢\tneng\t5000
够\tgou\t700000
量\tliang\t700000
你\tni\t700000
好\thao\t700000
你好\tni hao\t500000
";

fn tmp_db(suffix: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_sp_consume_{}_{}_{}.sqlite",
        suffix,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_file(&path);
    path
}

fn seeded_engine(suffix: &str) -> (Engine, PathBuf, PathBuf) {
    let db = tmp_db(suffix);
    let yaml = std::env::temp_dir().join(format!(
        "kime_sp_consume_seed_{}_{}.yaml",
        std::process::id(),
        suffix
    ));
    fs::write(&yaml, SEED).unwrap();
    let mut d = Dict::open(&db).unwrap();
    d.import(&yaml).unwrap();
    let e = Engine::new(
        d,
        Config {
            shuangpin: Some(Scheme::Ziranma),
            ..Config::default()
        },
    );
    (e, db, yaml)
}

fn ch(c: char) -> Key {
    Key {
        ch: Some(c),
        code: 0,
        shift: false,
        ctrl: false,
        alt: false,
    }
}

fn texts(e: &Engine) -> Vec<String> {
    e.candidates().iter().map(|c| c.text.clone()).collect()
}

/// 按候选文本选中（数字键 1-9 选页内第 1-9 个），返回上屏文本。
fn select_text(e: &mut Engine, text: &str) -> String {
    let list = texts(e);
    let idx = list
        .iter()
        .position(|t| t == text)
        .unwrap_or_else(|| panic!("候选里没有「{text}」：{list:?}"));
    assert!(
        idx < 9,
        "「{text}」落在第 {idx} 位，数字键（1-9）选不到：{list:?}"
    );
    let digit = (b'1' + idx as u8) as char;
    match e.key(ch(digit)) {
        Outcome::Commit(t) => t,
        other => panic!("选「{text}」期望 Commit，实际 {other:?}"),
    }
}

fn cleanup(db: &PathBuf, yaml: &PathBuf) {
    let _ = fs::remove_file(db);
    let _ = fs::remove_file(yaml);
}

#[test]
fn char_by_char_selection_keeps_following_syllables() {
    let (mut e, db, yaml) = seeded_engine("walk");
    for c in "ngld".chars() {
        e.key(ch(c));
    }
    assert_eq!(e.preedit(), "nengliang", "双拼 preedit 是解码后的拼音");
    assert!(texts(&e).contains(&"能量".to_string()));

    // 选首音节单字「能」：只吃掉 ng 两个键位，ld（量）必须留下。
    assert_eq!(select_text(&mut e, "能"), "能");
    assert_eq!(
        e.preedit(),
        "liang",
        "选「能」后应剩 liang（nd 键位），实际 {:?}",
        e.preedit()
    );
    assert!(
        texts(&e).contains(&"量".to_string()),
        "剩余音节应继续出候选「量」，实际 {:?}",
        texts(&e)
    );

    // 接着选「量」：整串吃完，组合清空。
    assert_eq!(select_text(&mut e, "量"), "量");
    assert!(e.preedit().is_empty(), "选完后 preedit 应清空");
    assert!(e.candidates().is_empty(), "选完后候选应清空");
    cleanup(&db, &yaml);
}

#[test]
fn three_syllable_string_keeps_middle_syllable() {
    let (mut e, db, yaml) = seeded_engine("mid");
    for c in "ngnihk".chars() {
        e.key(ch(c));
    }
    // 能+你+好：选「能」只吃 ng，nihk（你+好）整段保留。
    assert_eq!(select_text(&mut e, "能"), "能");
    assert_eq!(
        e.preedit(),
        "nihao",
        "中间的 ni 不该被 neng 的 4 个字母吃掉，实际 {:?}",
        e.preedit()
    );
    assert!(
        texts(&e).contains(&"你好".to_string()),
        "剩余音节应继续出候选，实际 {:?}",
        texts(&e)
    );
    cleanup(&db, &yaml);
}

#[test]
fn whole_word_selection_consumes_all_keys() {
    let (mut e, db, yaml) = seeded_engine("word");
    for c in "nggb".chars() {
        e.key(ch(c));
    }
    // 整词「能够」= 2 音节 = 4 键位，nggb 全部消耗（键位消耗不回退成旧行为）。
    assert_eq!(select_text(&mut e, "能够"), "能够");
    assert!(e.preedit().is_empty(), "整词上屏后 preedit 应清空");
    assert!(e.candidates().is_empty(), "整词上屏后候选应清空");
    cleanup(&db, &yaml);
}
