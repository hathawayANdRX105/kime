//! 英文候选的层序、按键消耗与噪声闸。
//!
//! 排序口径见 `crate::model::RankKey`：层（精确→补全）在分数之前，跨语言竞争发生在
//! 同层之内；两字母门槛不变。
//! `ok` / `an` 覆盖中英候选的层序；`skill` 覆盖英文候选在双拼下的按键消耗
//!（空格选中必须整词上屏并关闭面板）。

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::dict::{Candidate, Dict};
use kime_core::engine::KEY_SPACE;
use kime_core::Scheme;
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
skill	skill	900
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

/// 双拼（自然码）引擎：验证英文候选在双拼下的按键消耗。
fn sp_engine(tag: &str) -> (Engine, std::path::PathBuf) {
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
            shuangpin: Some(Scheme::Ziranma),
            ..Config::default()
        },
    );
    (e, dir)
}

/// 空格选中英文候选必须**一次上屏并关闭面板**。
///
/// 回归的 bug：英文候选的读音键是词本身（不是音节序列），旧实现按「音节个数 × 2 键」
/// 计算消耗，双拼下只吃掉 2 键，剩下的键留在组合里——面板不退出，下一次上屏变成
/// 「skill + 残余音节」的拼接串（用户实测：选 skill 后接着敲 ill，上屏 skill拆历来）。
#[test]
fn space_on_english_candidate_commits_and_closes_panel() {
    let (mut e, dir) = sp_engine("en_commit");
    type_letters(&mut e, "skill");
    assert!(
        e.candidates().iter().any(|c| c.text == "skill"),
        "候选里必须有英文 skill"
    );
    assert_eq!(
        e.key(Key {
            ch: None,
            code: KEY_SPACE,
            shift: false,
            ctrl: false,
            alt: false,
        }),
        Outcome::Commit("skill".into()),
        "空格选英文候选必须整词上屏"
    );
    assert!(e.preedit().is_empty(), "上屏后面板必须关闭");
    assert!(e.candidates().is_empty(), "上屏后候选必须清空");
    let _ = fs::remove_dir_all(dir);
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
