//! 每条切分都进词图（全拼路径的多切分组句）。
//!
//! 改前 `refresh_full_pinyin` 只对 `segs.first()` 跑一次 Viterbi：整串切分的第一条
//! 之外的真词/真句永远组不出来。`dangao` 贪心切成 `[dang,ao]`，真词「蛋糕」只在
//! `[dan,gao]` 里，首切分一次 Viterbi 拼不出它。改后每条长度 >= 2 的切分各跑一次，
//! 按切分顺序合并、按文本去重。
//!
//! 排序契约（B3 钉的四条 + fallback 回归）：
//! 1. 只有非首切分能组出的组句必须出现（改前恒空）；
//! 2. 首切分的句排在次切分的句之前（首切分走 place_sentences，次切分追加末尾）；
//! 3. 两条切分组出同文本句时，该文本只出现一次（保留更优先切分的）；
//! 4. 整串有精确词条时，组句（含次切分的）不得越过它；
//! 5. 半截尾巴整串切不出时仍走 fallback 组句（门控与语义未被 B3 改坏）。
//!
//! 夹具刻意把目标组句**只**放在 Viterbi 能拼出、而词级 `lookup_prefix` 拼不出的
//! 形态上：组句由两个独立单字（各自 pinyin 行）在词图里合出，词库里**没有**该
//! 拼写的整串词条——这样改前（首切分一次 Viterbi 必空）候选里必然没有它。

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::engine::{Engine, Key};
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn tmp_db(suffix: &str, rows: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_allseg_{}_{}_{}.sqlite",
        suffix,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_file(&path);
    let _ = Dict::open(&path).expect("create schema");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(rows).unwrap();
    drop(conn);
    path
}

fn engine(db: &Path) -> Engine {
    Engine::new(
        Dict::open(db).expect("open dict"),
        Config {
            dict_path: db.to_string_lossy().to_string(),
            ..Config::default()
        },
    )
}

fn keys(e: &mut Engine, s: &str) {
    for c in s.chars() {
        e.key(Key {
            ch: Some(c),
            code: 0,
            shift: false,
            ctrl: false,
            alt: false,
        });
    }
}

fn texts(e: &Engine) -> Vec<String> {
    e.candidates().iter().map(|c| c.text.clone()).collect()
}

fn position(got: &[String], text: &str) -> Option<usize> {
    got.iter().position(|t| t == text)
}

/// 目标「蛋糕」是 蛋+糕 两个单字在词图里合成的组句，词库里没有「蛋糕」整串词条。
///
/// 词库**只**有 dan/gao 两条单字行（没有 dang/ao 任何字），所以首切分 `[dang,ao]`
/// 一条词图边都建不出来，改前一次 Viterbi 必空；`[dan,gao]` 则能合成「蛋糕」。
#[test]
fn composed_only_by_other_segmentation_appears() {
    let db = tmp_db(
        "cake",
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('dan','蛋',9000000,'d',0),
           ('gao','糕',8000000,'g',0);",
    );
    let mut e = engine(&db);
    keys(&mut e, "dangao");
    let got = texts(&e);
    assert!(
        got.contains(&"蛋糕".to_string()),
        "只有次切分 [dan,gao] 能合成的组句「蛋糕」必须出现（改前只看首切分，必空），实际 {got:?}"
    );
    let _ = fs::remove_file(&db);
}

/// 同一个输入，首切分与次切分**各自**都能组出一句：B3 的顺序契约钉死
/// 「首切分的句排在次切分的句之前」。
///
/// `[dang,ao]` 用 当+奥 组出「当奥」（首切分，走 place_sentences）；`[dan,gao]` 用
/// 蛋+糕 组出「蛋糕」（次切分，追加到末尾）。两者都必须出现，且前者在前。
#[test]
fn first_segmentation_sentence_precedes_other_segmentations() {
    let db = tmp_db(
        "order",
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('dang','当',90000000,'da',0),
           ('ao','奥',80000000,'a',0),
           ('dan','蛋',9000000,'d',0),
           ('gao','糕',8000000,'g',0);",
    );
    let mut e = engine(&db);
    keys(&mut e, "dangao");
    let got = texts(&e);
    let first = position(&got, "当奥").expect("首切分 [dang,ao] 应合成「当奥」");
    let second = position(&got, "蛋糕").expect("次切分 [dan,gao] 应合成「蛋糕」");
    assert!(
        first < second,
        "首切分的句必须排在次切分的句之前，实际 {got:?}"
    );
    let _ = fs::remove_file(&db);
}

/// 去重：两条切分组出**同文本**句时，该文本在候选列表里只出现一次。
///
/// 「蛋」同时挂在 dan 与 dang，「糕」同时挂在 gao 与 ao：首切分 `[dang,ao]` 与
/// 次切分 `[dan,gao]` 各自都能合成「蛋糕」——同文本，必须去重保留先出现（更优先
/// 切分）的那条。若 B3 追加时漏了去重，这里会数出 2 条。
#[test]
fn same_text_from_two_segmentations_is_deduplicated() {
    let db = tmp_db(
        "dup",
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('dan','蛋',90000000,'d',0),
           ('dang','蛋',80000000,'da',0),
           ('gao','糕',70000000,'g',0),
           ('ao','糕',60000000,'a',0);",
    );
    let mut e = engine(&db);
    keys(&mut e, "dangao");
    let got = texts(&e);
    let hits = got.iter().filter(|t| *t == "蛋糕").count();
    assert_eq!(
        hits, 1,
        "两条切分组出同文本「蛋糕」时只许出现一次（保留更优先切分的），实际 {got:?}"
    );
    let _ = fs::remove_file(&db);
}

/// 层一精确命中不被抢：整串有精确词条「当奥」(dang'ao) 时，首切分合成的同文本被去
/// 重，次切分新合成的「蛋糕」只能追加到末尾——都不得越过层一首候选。
#[test]
fn layer_one_exact_hit_is_not_displaced_by_sentences() {
    let db = tmp_db(
        "layer1",
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('dang''ao','当奥',50000,'dao',0),
           ('dang','当',40000000,'da',0),
           ('ao','奥',30000000,'a',0),
           ('dan','蛋',9000000,'d',0),
           ('gao','糕',8000000,'g',0);",
    );
    let mut e = engine(&db);
    keys(&mut e, "dangao");
    let got = texts(&e);
    let exact = position(&got, "当奥").expect("整串精确词「当奥」必须在候选里");
    let second = position(&got, "蛋糕").expect("次切分 [dan,gao] 应合成「蛋糕」");
    assert_eq!(exact, 0, "整串精确命中恒为层一首候选，实际 {got:?}");
    assert!(
        second > exact,
        "次切分合成的句「蛋糕」必须追加在层一精确命中之后，不得抢位，实际 {got:?}"
    );
    let _ = fs::remove_file(&db);
}

/// 半截尾巴（整串切不出）仍走 fallback 组句：`shenmey` 的 y 敲了一半，segs 为空，
/// 靠完整音节 `[shen,me]` 合成「什么」。词库里**没有**「什么」整串词条（只有 什/么
/// 两个单字），所以「什么」只能由 fallback Viterbi 拼出——钉住门控与语义未被 B3 改坏。
#[test]
fn half_typed_tail_still_falls_back_to_completed_syllables() {
    let db = tmp_db(
        "tail",
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('shen','什',9000,'s',0),
           ('me','么',8000,'m',0);",
    );
    eprintln!(
        "[kime-diag-FALLTAIL] db={} rows={:?}",
        db.display(),
        kime_core::dict::Dict::open(&db)
            .unwrap()
            .conn()
            .prepare("SELECT pinyin||'='||text||'#'||abbrev FROM phrase ORDER BY pinyin")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .flatten()
            .collect::<Vec<_>>()
    );
    let mut e = engine(&db);
    eprintln!("[kime-diag-FALLTAIL] BEGIN fixture=什(shen)/么(me)");
    for c in "shenmey".chars() {
        let out = e.key(Key {
            ch: Some(c),
            code: 0,
            shift: false,
            ctrl: false,
            alt: false,
        });
        eprintln!(
            "[kime-diag] key {c:?} -> {out:?} letters={:?} cands={:?}",
            e.letters(),
            e.candidates()
                .iter()
                .map(|x| x.text.clone())
                .collect::<Vec<_>>()
        );
    }
    let got = texts(&e);
    assert!(
        got.contains(&"什么".to_string()),
        "半截尾巴（y 敲一半、整串切不出）必须仍由完整音节 [shen,me] 合成「什么」，实际 {got:?}"
    );
    let _ = fs::remove_file(&db);
}

/// 歧义单音节输入（`xian` → `[[xian],[xi,an]]`）的落位回归。
///
/// 首切分 `[xian]` 只有一个音节、不进 Viterbi；句来自后续切分 `[xi,an]`。
/// 此时查词键（`last_joined` = `xian`，层一精确块的依据）与句子覆盖键
/// （`xi'an`）**脱钩**——若两键混用，`l1_end` 被算成 0，句子插到层一精确块
/// **之前**，把「先」顶下首位。旧实现此输入根本不跑 Viterbi，故无此回归。
#[test]
fn ambiguous_single_syllable_sentence_lands_after_layer_one() {
    let db = tmp_db(
        "xian",
        "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
           ('xian','先',900000,'x',0),
           ('xian','现',800000,'x',0),
           ('xi','西',200000,'x',0),
           ('an','安',300000,'a',0);",
    );
    let mut e = engine(&db);
    keys(&mut e, "xian");
    let got = texts(&e);
    let xi_an = position(&got, "西安").expect("后续切分 [xi,an] 应合成「西安」");
    let first = position(&got, "先").expect("层一精确命中「先」必须在候选列");
    assert_eq!(
        first, 0,
        "层一精确命中必须居首，不能被后续切分的句顶掉，实际 {got:?}"
    );
    assert!(
        xi_an > first,
        "后续切分的句必须落在层一精确块之后（先/现都在 xian 块内），实际 {got:?}"
    );
    let _ = fs::remove_file(&db);
}
