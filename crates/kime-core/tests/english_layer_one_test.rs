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

/// 英文词表刻意放一条高频词（zebra 900000）当基准：跨语言分数是「在本语言分布里的
/// 相对位置」，若被测词恰好是词表最大频，它会并列满分并按文本序胜出，断言就不再
/// 表达任何契约（小词表上的历史误判来源）。
const EN: &str = "\
...
OK	OK	10
okay	okay	500000
an	an	20
skill	skill	900
zebra	zebra	900000
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

/// 排序键是 `(层, 校准分数, 文本, 来源)`：**层在分数之前，跨语言竞争只发生在同层内**。
///
/// 输入 `ok`：中文只有补全（哦可 9M / 哦可哦可 8M，层二）；英文有精确词 OK（层一）
/// 与补全词 okay（层二）。因此：
///
/// - 英文精确 OK（层一）排在中文补全之前——层一整体在层二之前，与语言无关；
/// - 英文补全 okay（500k）与中文补全 哦可（9M）同层比分，哦可更高，故在前。
#[test]
fn layer_one_beats_layer_two_across_languages() {
    let (mut e, dir) = engine("ok");
    type_letters(&mut e, "ok");
    let got = texts(e.candidates());
    let pos = |t: &str| got.iter().position(|x| *x == t).unwrap();
    let (ok, oke, okeoke, okay) = (pos("OK"), pos("哦可"), pos("哦可哦可"), pos("okay"));
    assert!(
        ok < oke,
        "层一（英文精确）整体在层二（中文补全）之前：{got:?}"
    );
    assert!(ok < okay, "英文精确仍先于英文补全：{got:?}");
    assert!(oke < okay, "同层比分：900k 应压过 500k：{got:?}");
    assert!(
        oke < okeoke,
        "同层同分文本升序前的频率序：9M 在 8M 之前：{got:?}"
    );
    fs::remove_dir_all(dir).unwrap();
}

/// 输入 `an`：中文层一 安(9M)/按(8M)，英文层一 an(20，英文表基准 900k)，
/// 中文层二 安排。
///
/// 期望 `安 按 an 安排`：
/// - 层一内部按分数：安 > 按 > an（an 在本语言分布里位置很低）；
/// - 层二整体在层一之后：安排 排在 an 之后——**层序压制分数**，不是「谁分高谁在前」。
#[test]
fn layer_order_beats_score_across_languages() {
    let (mut e, dir) = engine("an");
    type_letters(&mut e, "an");
    let got = texts(e.candidates());
    assert_eq!(
        got,
        ["安", "按", "an", "安排"],
        "层一（安/按/an）整体在层二（安排）之前：{got:?}"
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
