//! 邻键纠错（全拼）验收测试。
//!
//! 钉住三件事：
//! 1. 相邻转位——`xain` 出 xian 词（转位是拼音最经典的错法，且 `xain` segment
//!    失败，只能靠按键串层纠错）；
//! 2. 邻键替换——`zhant`（t/g 相邻）出 zhang 词；
//! 3. 防误纠——候选充足的正常输入，correction 开关两档的候选序完全一致；
//!    且纠错候选不抢精确候选的位。

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::engine::{Engine, Key, Outcome};

fn tmp_db(suffix: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_correction_{}_{}_{}.sqlite",
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

/// 词库种子：xian/zhang 各给足够词，外加一个纯高频词做精确排位对照。
const SEED: &str = "\
...
先\txian\t50000
现\txian\t40000
线\txian\t30000
县\txian\t20000
宪\txian\t10000
张\tzhang\t50000
章\tzhang\t40000
掌\tzhang\t30000
涨\tzhang\t20000
帐\tzhang\t10000
你\tni\t90000
拟\tni\t80000
逆\tni\t70000
腻\tni\t60000
溺\tni\t50000
泥\tni\t40000
";

fn seeded_engine(suffix: &str, correction: bool) -> (Engine, PathBuf, PathBuf) {
    let db = tmp_db(suffix);
    let yaml = std::env::temp_dir().join(format!(
        "kime_correction_seed_{}_{}.yaml",
        std::process::id(),
        suffix
    ));
    fs::write(&yaml, SEED).unwrap();
    let mut d = Dict::open(&db).unwrap();
    d.import(&yaml).unwrap();
    let e = Engine::new(
        d,
        Config {
            shuangpin: None,
            correction,
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

/// 逐键喂入（字母），返回最终候选文本列表。
fn type_keys(e: &mut Engine, letters: &str) -> Vec<String> {
    for c in letters.chars() {
        if let Outcome::Commit(text) = e.key(ch(c)) {
            return vec![format!("<commit:{text}>")];
        }
    }
    e.candidates().iter().map(|c| c.text.clone()).collect()
}

fn cleanup(db: &PathBuf, yaml: &PathBuf) {
    let _ = fs::remove_file(db);
    let _ = fs::remove_file(yaml);
}

#[test]
fn transposition_corrects_xain_to_xian_words() {
    let (mut e, db, yaml) = seeded_engine("transp", true);
    // `xain` 切不出音节（segment 失败），直查必空；纠错应转位出 xian。
    let cands = type_keys(&mut e, "xain");
    assert!(
        cands.contains(&"先".to_string()) && cands.contains(&"线".to_string()),
        "xain 应纠错出 xian 词，实际: {cands:?}"
    );
    cleanup(&db, &yaml);
}

#[test]
fn substitution_corrects_zhant_to_zhang_words() {
    let (mut e, db, yaml) = seeded_engine("subst", true);
    // t 与 g 物理相邻：zhant → zhang。
    let cands = type_keys(&mut e, "zhant");
    assert!(
        cands.contains(&"张".to_string()) && cands.contains(&"掌".to_string()),
        "zhant 应纠错出 zhang 词，实际: {cands:?}"
    );
    cleanup(&db, &yaml);
}

#[test]
fn rich_query_is_identical_with_correction_on_and_off() {
    let (mut e_on, db1, yaml1) = seeded_engine("rich_on", true);
    let (mut e_off, db2, yaml2) = seeded_engine("rich_off", false);
    let on = type_keys(&mut e_on, "ni");
    let off = type_keys(&mut e_off, "ni");
    assert!(
        on.len() >= 5,
        "夹具应让 ni 直查候选充足（否则触发条件失效），实际 {on:?}"
    );
    assert_eq!(on, off, "候选充足时纠错不得改变任何候选与顺序");
    cleanup(&db1, &yaml1);
    cleanup(&db2, &yaml2);
}

#[test]
fn correction_off_leaves_typos_uncorrected() {
    let (mut e, db, yaml) = seeded_engine("off", false);
    let cands = type_keys(&mut e, "xain");
    assert!(
        !cands.contains(&"先".to_string()),
        "correction=false 时 xain 不得纠错，实际: {cands:?}"
    );
    cleanup(&db, &yaml);
}
