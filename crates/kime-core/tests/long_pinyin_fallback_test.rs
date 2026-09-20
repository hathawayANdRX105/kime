//! 长串拼音的候选被整句占满但不足一页时，首音节单字候选追加到末尾：
//! 整句在前、单字在后，翻页翻得到「我」，选完能继续组词。
//!
//! 之前只在候选**完全为空**时才回退（`longest_prefix_candidates`），
//! `womendoubuzhidao` 有整句候选就被锁死，翻页翻不到任何单字。

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::engine::{Engine, Key, Outcome};
use std::fs;
use std::path::PathBuf;

/// 一条整句 + 首音节 3 个单字：整句候选存在但不足一页（page_size 2），
/// 正是降级触发条件。追加后 4 条候选跨 2 页，末页只剩单字——翻页降级可观察。
/// 其余音节每节只放一个字，避免 Viterbi 用它们拼出别的整句占据追加位
/// （早期 fixture 放多个 wo 单字时拼出了「卧们都不知道」）。
const CN: &str = "...\n我\two\n握\two\n窝\two\n们\tmen\n都\tdou\n不\tbu\n知\tzhi\n道\tdao\n我们都不知道\two men dou bu zhi dao\n";

fn tmp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "kime_longfb_{tag}_{}_{}",
        std::process::id(),
        nanos
    ))
}

/// page_size 2：追加后 4 条候选跨 2 页，末页只剩单字。
fn engine_at(tag: &str) -> (Engine, PathBuf) {
    let dir = tmp_dir(tag);
    fs::create_dir_all(&dir).unwrap();
    let yaml = dir.join("cn.dict.yaml");
    fs::write(&yaml, CN).unwrap();
    let mut dict = Dict::open(dir.join("dict.sqlite3")).unwrap();
    dict.import(&yaml).unwrap();
    let e = Engine::new(
        dict,
        Config {
            page_size: 2,
            ..Config::default()
        },
    );
    (e, dir)
}

fn k(c: char) -> Key {
    Key {
        ch: Some(c),
        code: 0,
        shift: false,
        ctrl: false,
        alt: false,
    }
}

fn code_k(code: u32) -> Key {
    Key {
        ch: None,
        code,
        shift: false,
        ctrl: false,
        alt: false,
    }
}

const KEY_SPACE: u32 = 57;
const KEY_EQUAL: u32 = 13;

#[test]
fn long_pinyin_sentence_list_falls_back_to_first_syllable_chars() {
    let (mut e, dir) = engine_at("main");
    for c in "womendoubuzhidao".chars() {
        e.key(k(c));
    }
    let cs: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
    assert_eq!(
        cs,
        vec!["我们都不知道", "我", "握", "窝"],
        "整句在前、首音节单字追加到末尾，实际 {cs:?}"
    );
    // 空格首选不变：仍是整句首候选（追加的单字不污染层一/整句名次）
    match e.key(code_k(KEY_SPACE)) {
        Outcome::Commit(t) => assert_eq!(t, "我们都不知道"),
        other => panic!("expected Commit, got {:?}", other),
    }
    assert!(e.preedit().is_empty() && e.candidates().is_empty());

    // 重新打长串，翻到末页选单字「窝」——追加的单字是一等候选
    for c in "womendoubuzhidao".chars() {
        e.key(k(c));
    }
    assert_eq!(e.page(), (0, 2));
    assert_eq!(e.key(code_k(KEY_EQUAL)), Outcome::Consumed);
    assert_eq!(e.page(), (1, 2), "末页应只剩追加的首音节单字");
    match e.key(k('1')) {
        Outcome::Commit(t) => assert_eq!(t, "握"),
        other => panic!("expected Commit(握), got {:?}", other),
    }

    // 选了单字后继续组词：组合已清空，接着打能出候选
    assert!(e.preedit().is_empty());
    for c in "men".chars() {
        e.key(k(c));
    }
    assert!(
        e.candidates().iter().any(|c| c.text == "们"),
        "选单字后应能继续组词，实际 {:?}",
        e.candidates()
            .iter()
            .map(|c| c.text.clone())
            .collect::<Vec<_>>()
    );
    let _ = fs::remove_dir_all(&dir);
}
