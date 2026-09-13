//! 英文词候选：独立表、独立匹配路径。
//!
//! 契约（对齐 rime 的 english translator）：拿**原始按键串**匹配 `english` 表，不做拼音解码；
//! 精确命中先于补全（层一整体在前），层内 `(freq DESC, text ASC)`。
//!
//! 最容易被写坏的地方是**污染**：英文条目一旦混进 `phrase`，打 `he` 就会在中文候选里
//! 捞出 `help`。这里既测「英文查得到」，也测「中文查不到英文」，且 FST 与纯 SQLite
//! 两条路径都跑一遍。

use std::fs;
use std::path::Path;

use kime_core::builder::build;
use kime_core::dict::{Candidate, Dict};

/// 中文表：`he`/`shi` 与英文前缀同形，是污染最容易暴露的输入。
const CN: &str = "\
...
喝	he	500
河	he	400
是	shi	9000
世界	shi jie	800
";

/// 英文表：精确词的频率**故意低于**它的补全词（help 10 vs helper 900），
/// 这样「精确优先」一旦退化成全表频率排序，首候选立刻换人。
/// 尾部几条是 rime-ice 真实存在的脏行：`#` 注释词、带 `'` 的同形别名、带 `.`/空格
/// 的条目、非 ASCII 词 —— 全部不该入库。
const EN: &str = "\
...
help	help	10
helper	helper	900
helpful	helpful	800
hell	hell	800
hello	hello	10
hellos	hellos	500
world	world	700
AA	AA	50
aa	aa	20
never	never	3
# a	a
he'll	hell	1
.NET	net	1
iPhone 17	iPhone	4
café	cafe	1
";

/// 建库 → 导中文 + 英文 → （可选）烘 dict.bin → 重新开库（英文必须活过重启）。
fn seeded(dir: &Path, with_bin: bool) -> Dict {
    let cn = dir.join("cn.yaml");
    let en = dir.join("en.yaml");
    fs::write(&cn, CN).unwrap();
    fs::write(&en, EN).unwrap();
    let db = dir.join("dict.sqlite3");
    {
        let mut seed = Dict::open(&db).unwrap();
        seed.import(&cn).unwrap();
        assert_eq!(seed.import_english(&en).unwrap(), 10, "脏行不该入库");
        assert_eq!(
            seed.import_english(&en).unwrap(),
            0,
            "同文件重复导入应幂等（频率取大，不再写行）"
        );
    }
    if with_bin {
        build(&db, &dir.join("dict.bin")).unwrap();
    }
    Dict::open(&db).unwrap()
}

fn texts(hits: &[Candidate]) -> Vec<&str> {
    hits.iter().map(|c| c.text.as_str()).collect()
}

/// 中文三条查询路径都不得出现英文词。
fn assert_chinese_clean(d: &Dict) {
    for (label, hits) in [
        ("lookup(he)", d.lookup(&["he".into()], 20).unwrap()),
        (
            "lookup_prefix(he)",
            d.lookup_prefix(&["he".into()], "", 20).unwrap(),
        ),
        (
            "lookup_prefix(h,e)",
            d.lookup_prefix(&["h".into()], "e", 20).unwrap(),
        ),
        ("lookup_abbrev(h)", d.lookup_abbrev("h", 20).unwrap()),
        ("lookup(hello)", d.lookup(&["hello".into()], 20).unwrap()),
    ] {
        let got = texts(&hits);
        assert!(
            got.iter()
                .all(|t| t.chars().all(|c| !c.is_ascii_alphabetic())),
            "{label} 混进了英文词: {got:?}"
        );
    }
    assert!(
        d.lookup(&["hello".into()], 20).unwrap().is_empty(),
        "english 表的内容不该出现在 phrase 的拼音精确查询里"
    );
}

#[test]
fn exact_hit_beats_higher_freq_completion() {
    for with_bin in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let d = seeded(dir.path(), with_bin);

        assert_eq!(
            texts(&d.lookup_english("hello", 10)),
            vec!["hello", "hellos"],
            "精确命中必须先于补全，哪怕 hellos 频率更高"
        );
        assert_eq!(
            texts(&d.lookup_english("help", 10)),
            vec!["help", "helper", "helpful"],
            "精确命中 help 不该被 helper(900)/helpful(800) 压住"
        );
    }
}

#[test]
fn prefix_hits_come_back_in_layer_freq_order() {
    let dir = tempfile::tempdir().unwrap();
    let d = seeded(dir.path(), false);

    // `hel` 无精确条目 → 整段是层二，纯 (freq DESC, text ASC)：
    // helper(900) > hell(800) = helpful(800)（同频按文本升序）> hellos(500) > help(10) = hello(10)
    assert_eq!(
        texts(&d.lookup_english("hel", 10)),
        vec!["helper", "hell", "helpful", "hellos", "hello", "help"]
    );
    // limit 生效，且不回填
    assert_eq!(texts(&d.lookup_english("hel", 2)), vec!["helper", "hell"]);
    // 层一名额优先：limit=1 时补全一个都拿不到
    assert_eq!(texts(&d.lookup_english("hell", 1)), vec!["hell"]);
}

#[test]
fn unmatched_input_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let d = seeded(dir.path(), false);

    assert!(d.lookup_english("zzzz", 10).is_empty());
    assert!(d.lookup_english("helpp", 10).is_empty(), "比命中长一截该空");
    assert!(d.lookup_english("", 10).is_empty(), "空串不查");
    assert!(d.lookup_english("help", 0).is_empty(), "limit 0 不查");
}

#[test]
fn junk_rows_are_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let d = seeded(dir.path(), false);

    // `# a` 没被收进来：查 `a` 只得到 aa 系（层二），没有精确的 `a`
    assert_eq!(texts(&d.lookup_english("a", 10)), vec!["AA", "aa"]);
    // `he'll` / `.NET` / `iPhone 17` / `café` 都不在库里
    assert_eq!(
        texts(&d.lookup_english("he", 20)),
        vec!["helper", "hell", "helpful", "hellos", "hello", "help"]
    );
    assert_eq!(texts(&d.lookup_english("net", 10)), Vec::<&str>::new());
    assert_eq!(texts(&d.lookup_english("ne", 10)), vec!["never"]);
}

#[test]
fn lookup_is_case_insensitive_but_keeps_word_case() {
    let dir = tempfile::tempdir().unwrap();
    let d = seeded(dir.path(), false);

    // AA(50) 与 aa(20) 是两条独立条目：主键是原文，互不覆盖，都靠小写键命中。
    assert_eq!(texts(&d.lookup_english("aa", 10)), vec!["AA", "aa"]);
    assert_eq!(texts(&d.lookup_english("AA", 10)), vec!["AA", "aa"]);
    let hits = d.lookup_english("aa", 10);
    assert_eq!(hits[0].pinyin, "AA", "pinyin 填词本身（主控分层判断用）");
    assert!(hits.iter().all(|c| !c.ai));
}

#[test]
fn english_does_not_pollute_chinese_lookups() {
    for with_bin in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let d = seeded(dir.path(), with_bin);
        assert_chinese_clean(&d);
        // 对照组：中文查询本身照常工作，别把「干净」实现成「全空」
        assert_eq!(
            texts(&d.lookup(&["he".into()], 10).unwrap()),
            vec!["喝", "河"]
        );
    }
}
