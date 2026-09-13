//! 英文精确词的层归属（第四轮反馈第 1 条）：`ok` 被「哦可哦可」压住、`emoji` 看不见。
//!
//! 契约：英文候选里**精确消耗输入的词条**（pinyin == 原始输入，大小写不敏感——
//! pinyin 就是词本身）与中文精确命中同属层一：插到中文层一块之后，中文层一空时打头。
//! 前缀补全的英文仍挂全部中文（含层二补全）之后。回归对象就是旧的「一律 extend 到
//! 末尾」实现：把它改回去，下面的断言逐条炸。

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::dict::{Candidate, Dict};
use kime_core::{Engine, Key, Outcome};

/// 线上事故形态：打 `ok` 时中文只有层二补全（哦可 o'ke、哦可哦可 o'ke'o'ke，频率一个比一个高），
/// 精确英文词 OK 频率再低也必须排它们前面。`an` 一则是中文层一非空：安/按 之后才是 an，再之后才是补全。
const CN: &str = "\
...
哦	o	3000000
哦可	o ke	9000000
哦可哦可	o ke o ke	8000000
安	an	9000000
按	an	8000000
安排	an pai	7000000
";

/// OK 是精确词但频率最低——精确优先一旦退化成频率序，它立刻被 okay/哦可 顶掉。
const EN: &str = "\
...
OK	OK	10
okay	okay	500000
an	an	20
";

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kime_enl1_{}_{}_{}",
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

fn engine(tag: &str) -> (Engine, std::path::PathBuf) {
    let dir = tmp_dir(tag);
    let cn = dir.join("cn.yaml");
    let en = dir.join("en.yaml");
    fs::write(&cn, CN).unwrap();
    fs::write(&en, EN).unwrap();
    let mut dict = Dict::open(dir.join("dict.sqlite3")).unwrap();
    dict.import(&cn).unwrap();
    dict.import_english(&en).unwrap();
    let e = Engine::new(
        dict,
        Config {
            shuangpin: None,
            ..Config::default()
        },
    );
    (e, dir)
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

fn texts(cands: &[Candidate]) -> Vec<&str> {
    cands.iter().map(|c| c.text.as_str()).collect()
}

/// 中文层一空（`ok` 没有任何词读音恰为 o'k）：英文精确词 OK 打头，
/// 补全词 okay 仍排在所有中文之后（层二末尾）。
#[test]
fn english_exact_leads_when_chinese_layer_one_is_empty() {
    let (mut e, dir) = engine("ok");
    type_letters(&mut e, "ok");
    let got = texts(e.candidates());
    assert_eq!(
        got.first().copied(),
        Some("OK"),
        "英文精确词必须打头，实际列表 {got:?}"
    );
    let ok_pos = got.iter().position(|t| *t == "OK").unwrap();
    let oke_pos = got
        .iter()
        .position(|t| *t == "哦可")
        .expect("哦可 补全在场");
    let okay_pos = got.iter().position(|t| *t == "okay").unwrap();
    assert!(
        ok_pos < oke_pos,
        "精确 OK 不许被层二补全 哦可 压住：{got:?}"
    );
    assert!(
        okay_pos > oke_pos,
        "英文补全 okay 必须仍挂在中文之后：{got:?}"
    );
    fs::remove_dir_all(dir).unwrap();
}

/// 中文层一非空（`an`：安/按 精确命中）：顺序 = 中文层一块 → 英文精确词 an → 层二补全。
/// 插入点用 engine 记住的 joined key，与 place_sentences 的层一边界同一约定。
#[test]
fn english_exact_sits_after_chinese_layer_one_block() {
    let (mut e, dir) = engine("an");
    type_letters(&mut e, "an");
    let got = texts(e.candidates());
    let pai_pos = got.iter().position(|t| *t == "安排").unwrap();
    let en_pos = got
        .iter()
        .position(|t| *t == "an")
        .expect("英文精确词 an 在场");
    assert_eq!(&got[..2], ["安", "按"], "中文层一块必须原样打头：{got:?}");
    assert!(
        en_pos > 1 && en_pos < pai_pos,
        "英文精确词必须在中文层一之后、层二补全 安排 之前：{got:?}"
    );
    fs::remove_dir_all(dir).unwrap();
}

/// 单字母不查英文（既有噪声闸不变）；两个字母起精确词才进层一。
#[test]
fn single_letter_still_skips_english() {
    let (mut e, dir) = engine("a");
    type_letters(&mut e, "a");
    let got = texts(e.candidates());
    assert!(!got.contains(&"a"), "单字母不该把英文噪声灌进候选：{got:?}");
    fs::remove_dir_all(dir).unwrap();
}
