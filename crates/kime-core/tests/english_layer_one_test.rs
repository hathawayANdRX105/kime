//! 英文精确词优先于中文补全（例如 `ok` 的 OK），但中文精确命中仍在最前。
//!
//! 契约：英文精确匹配插到中文层一之后、中文补全之前；英文前缀补全仍放在末尾，
//! 两字母门槛不变。
//! `ok` 与 `an` 回归覆盖精确和补全之间的层级顺序。

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

/// 中文层一空（`ok` 没有任何词读音恰为 o'k）：英文精确命中 OK 插到层一块之后、
/// 中文补全块之前，即整体打头；英文块内精确 OK 仍先于补全 okay。
#[test]
fn english_exact_leads_when_chinese_layer_one_empty() {
    let (mut e, dir) = engine("ok");
    type_letters(&mut e, "ok");
    let got = texts(e.candidates());
    let ok_pos = got.iter().position(|t| *t == "OK").unwrap();
    let oke_pos = got.iter().position(|t| *t == "哦可").unwrap();
    let okay_pos = got.iter().position(|t| *t == "okay").unwrap();
    assert_eq!(
        got.first().copied(),
        Some("OK"),
        "英文精确 OK 应打头，实际列表 {got:?}"
    );
    assert!(
        ok_pos < oke_pos,
        "英文精确 OK 不得落后于中文补全 哦可：{got:?}"
    );
    assert!(ok_pos < okay_pos, "英文块内精确仍先于补全：{got:?}");
    fs::remove_dir_all(dir).unwrap();
}

/// 中文精确命中仍先于英文精确词，英文精确词再先于中文补全。
#[test]
fn chinese_exact_then_english_exact_then_chinese_completion() {
    let (mut e, dir) = engine("an");
    type_letters(&mut e, "an");
    let got = texts(e.candidates());
    assert_eq!(
        got,
        ["安", "按", "an", "安排"],
        "中文精确 → 英文精确 → 中文补全：{got:?}"
    );
    fs::remove_dir_all(dir).unwrap();
}
/// 单字母不查英文（既有噪声闸不变）；两个字母起精确词才进候选。
#[test]
fn single_letter_still_skips_english() {
    let (mut e, dir) = engine("a");
    type_letters(&mut e, "a");
    let got = texts(e.candidates());
    assert!(!got.contains(&"a"), "单字母不该把英文噪声灌进候选：{got:?}");
    fs::remove_dir_all(dir).unwrap();
}
