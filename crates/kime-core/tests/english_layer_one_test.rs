//! 中文候选统一置于英文候选之前（例如 `ok`：哦可、哦可哦可 在 OK、okay 之前），
//!
//! 中文内部保持精确→补全，英文内部保持精确→补全；两字母门槛不变。
//! `ok` 与 `an` 回归覆盖中文整块在前、英文块内层序。

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::dict::{Candidate, Dict};
use kime_core::{Engine, Key, Outcome};

/// `ok` 时中文只有层二补全（哦可 o'ke、哦可哦可 o'ke'o'ke）：中文整块在英文块之前。
/// `an` 时中文层一非空（安/按/安排），英文候选落在中文整块之后。
const CN: &str = "\
...
哦	o	3000000
哦可	o ke	9000000
哦可哦可	o ke o ke	8000000
安	an	9000000
按	an	8000000
安排	an pai	7000000
";

/// 英文块只含 exact（OK/an）与补全（okay），内部精确优先于补全，整块接在中文候选之后。
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

/// 中文层一为空（`ok` 无读音恰为 o'k 的词）：中文补全块（哦可、哦可哦可）在英文块之前；
/// 英文块内精确 OK 仍先于补全 okay。
#[test]
fn chinese_leads_english_when_layer_one_empty() {
    let (mut e, dir) = engine("ok");
    type_letters(&mut e, "ok");
    let got = texts(e.candidates());
    let ok_pos = got.iter().position(|t| *t == "OK").unwrap();
    let oke_pos = got.iter().position(|t| *t == "哦可").unwrap();
    let okeoke_pos = got.iter().position(|t| *t == "哦可哦可").unwrap();
    let okay_pos = got.iter().position(|t| *t == "okay").unwrap();
    // 中文候选统一在英文候选之前；英文块内精确仍先于补全。
    assert!(oke_pos < ok_pos, "中文补全 哦可 不得落后于英文 OK：{got:?}");
    assert!(
        okeoke_pos < ok_pos,
        "中文补全 哦可哦可 不得落后于英文 OK：{got:?}"
    );
    assert!(ok_pos < okay_pos, "英文块内精确仍先于补全：{got:?}");
    fs::remove_dir_all(dir).unwrap();
}

/// 中文层一非空（安/按）：中文精确 → 中文补全（安排）→ 英文精确（an），
/// 即中文整块在英文候选之前，英文块内只有一条 exact。
#[test]
fn chinese_exact_then_completion_then_english() {
    let (mut e, dir) = engine("an");
    type_letters(&mut e, "an");
    let got = texts(e.candidates());
    assert_eq!(
        got,
        ["安", "按", "安排", "an"],
        "中文整块在英文候选之前：{got:?}"
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
