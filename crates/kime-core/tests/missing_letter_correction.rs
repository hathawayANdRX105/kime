//! 缺字母（单字母插入）纠错 + 缺陷 A 头部回退（全拼）验收测试。
//!
//! 钉住 #111：`nizoba`（想打 `nizouba` 你走吧 / `nizuoba` 你坐吧，漏了 u）
//! 0 候选——`zo` 不是合法音节，整串不可切分，替换/转位纠错也够不到原词。
//! 本次新增第三类纠错「缺字母插入」，并在纯全拼模式下把缺陷 A 头部回退
//! 扩展到退化切分的完整音节段：
//! 1. `nizoba`（correction on）→ 出「你走吧」（插入 u 成 zuo/zou 才可切分）；
//! 2. `zoba`（correction on）→ 两音节短串同样可救；
//! 3. `nizoba`（correction off）→ 至少给出首音节单字「你」（且不漏「你走吧」）；
//! 4. `inserted_keys` 确定性、去重与输入守卫。

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::correction::inserted_keys;
use kime_core::dict::Dict;
use kime_core::engine::{Engine, Key, Outcome};

fn tmp_db(suffix: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_missing_{}_{}_{}.sqlite",
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

/// 词库种子（真实词库频率，拼音空格分隔音节）：ni / zou / zuo / ba 单字
/// + 两音节、三音节组合词，缺字母插入与缺陷 A 回退各有断言落点。
const SEED: &str = "\
...
你\tni\t1422456
尼\tni\t80000
呢\tni\t70000
泥\tni\t60000
溺\tni\t50000
走\tzou\t160000
奏\tzou\t30000
坐\tzuo\t1106273
作\tzuo\t200000
吧\tba\t2403359
八\tba\t740031
走吧\tzou ba\t87156
你走吧\tni zou ba\t8090
坐吧\tzuo ba\t6406
作罢\tzuo ba\t17332
";

fn seeded_engine(suffix: &str, correction: bool) -> (Engine, PathBuf, PathBuf) {
    let db = tmp_db(suffix);
    let yaml = std::env::temp_dir().join(format!(
        "kime_missing_seed_{}_{}.yaml",
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
fn missing_u_recovers_word_from_uncuttable_input() {
    let (mut e, db, yaml) = seeded_engine("nizoba", true);
    // `zo` 不是合法音节：`nizoba` 整串切不出音节，主查询必 0；
    // 只有插入 u 使其可切分（nizuoba / nizouba）重查才出「你走吧」。
    let cands = type_keys(&mut e, "nizoba");
    assert!(
        cands.contains(&"你走吧".to_string()),
        "nizoba 应经单字母插入纠错出 你走吧，实际: {cands:?}"
    );
    cleanup(&db, &yaml);
}

#[test]
fn missing_u_two_syllable_tail() {
    let (mut e, db, yaml) = seeded_engine("zoba", true);
    // 两音节短串：`zoba` 插入 u 得 zuoba（坐吧/作罢）与 zouba（走吧）。
    let cands = type_keys(&mut e, "zoba");
    assert!(
        cands.contains(&"坐吧".to_string()),
        "zoba 应插入 u 成 zuoba 出 坐吧，实际: {cands:?}"
    );
    assert!(
        cands.contains(&"走吧".to_string()),
        "zoba 应插入 u 成 zouba 出 走吧，实际: {cands:?}"
    );
    cleanup(&db, &yaml);
}

#[test]
fn uncuttable_input_still_offers_head_chars() {
    let (mut e, db, yaml) = seeded_engine("uncut", false);
    // correction off：插入路径不可用，「你走吧」不可达；但纯全拼模式下
    // 缺陷 A 回退应给出退化切分首音节（ni）的单字候选。
    let cands = type_keys(&mut e, "nizoba");
    assert!(
        cands.contains(&"你".to_string()),
        "nizoba（correction off）至少应出首音节 你，实际: {cands:?}"
    );
    assert!(
        !cands.contains(&"你走吧".to_string()),
        "correction off 不应出现 你走吧，实际: {cands:?}"
    );
    cleanup(&db, &yaml);
}

#[test]
fn inserted_keys_deterministic_and_guarded() {
    let a = inserted_keys("zo");
    let b = inserted_keys("zo");
    assert!(
        a.iter().any(|s| s == "zuo") && a.iter().any(|s| s == "zou"),
        "inserted_keys(\"zo\") 应含 zuo/zou，实际: {a:?}"
    );
    assert_eq!(a, b, "两次调用必须逐元素一致（枚举序确定性）");
    let mut uniq = a.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), a.len(), "不得有重复串: {a:?}");
    assert!(inserted_keys("").is_empty(), "空串 → 空");
    assert!(inserted_keys("a1").is_empty(), "含非 a-z → 空");
}
