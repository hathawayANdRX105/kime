//! 层序穿过合并阶段（工单第 4 条的 dict 侧契约）。
//!
//! `lookup_prefix` 返回的两层结构——层一（精确命中 joined）整体在前、层二（补全）
//! 在后——必须扛得住用户词 overlay 合并：
//! 1. 用户词在场会触发合并排序，排序只许发生在**层内**；「层一低频精确词 +
//!    层二高频补全词」的种子下，精确词必须仍然排第一。把 `merge_overlay` 退回
//!    跨层整体 `(freq DESC, text ASC)` 的本测试即失败。
//! 2. 用户词与层一基底同文本时：频率以用户词为准、层归属沿用原条目，
//!    不许在层二复制一份。
//! 3. 用户词本身是精确命中 → 进层一，哪怕层二躺着同文本的高频词库词。
//!
//! FST 与纯 SQLite 内存索引两条路径必须同语义。

use std::fs;
use std::path::Path;

use kime_core::builder::build;
use kime_core::dict::{Candidate, Dict};

/// 种子：层一只有低频精确词「精确」(10)，层二是高频补全词「高频」(900000)。
const SEED: &str = "...\n精确\tni hao\t10\n高频\tni hao ma\t900000\n";

fn texts(hits: &[Candidate]) -> Vec<&str> {
    hits.iter().map(|c| c.text.as_str()).collect()
}

/// `seed` 导入 SQLite（dir/dict.sqlite3）；`with_bin` 时再编译 dict.bin 走 FST 路径。
fn dict(dir: &Path, seed: &str, with_bin: bool) -> Dict {
    let yaml = dir.join("seed.yaml");
    fs::write(&yaml, seed).unwrap();
    let db = dir.join("dict.sqlite3");
    let mut d = Dict::open(&db).unwrap();
    d.import(&yaml).unwrap();
    if with_bin {
        drop(d);
        build(&db, &dir.join("dict.bin")).unwrap();
        Dict::open(&db).unwrap()
    } else {
        d
    }
}

/// 用户词在场（精确命中被学成用户词）时，层一低频词仍不得被层二高频补全压住。
#[test]
fn user_overlay_merge_keeps_exact_layer_ahead_of_completions() {
    for with_bin in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let tag = if with_bin { "FST" } else { "SQLite" };
        let mut d = dict(dir.path(), SEED, with_bin);
        // 学一次「精确」：它以用户词身份（freq 11）参与层一合并，触发排序分支。
        d.learn(&["ni".into(), "hao".into()], "精确").unwrap();

        let hits = d.lookup_prefix(&["ni".into()], "hao", 10).unwrap();
        assert_eq!(
            texts(&hits),
            vec!["精确", "高频"],
            "{tag}: 层一低频精确词被跨层频率排序打平了"
        );
        assert_eq!(
            hits[0].pinyin, "ni'hao",
            "{tag}: 首个候选必须是精确命中，实际 {:?}",
            hits[0].pinyin
        );
    }
}

/// 用户词（长 key 下学的）与层一基底同文本：频率走用户词，层归属不变，层二不复制。
#[test]
fn user_word_cannot_jump_layers_via_same_text() {
    let seed = "...\n重复\tni hao\t10\n重复\tni hao ma\t900000\n";
    for with_bin in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let tag = if with_bin { "FST" } else { "SQLite" };
        let mut d = dict(dir.path(), seed, with_bin);
        // 「重复」在长 key `ni'hao'ma` 下被学习（freq 900000 → 900001, user=1）。
        d.learn(&["ni".into(), "hao".into(), "ma".into()], "重复")
            .unwrap();

        let hits = d.lookup_prefix(&["ni".into()], "hao", 10).unwrap();
        assert_eq!(
            texts(&hits),
            vec!["重复"],
            "{tag}: 用户词在层二复制了一份/或让高频基底条目越层插队，实际 {hits:?}"
        );
        assert_eq!(hits[0].pinyin, "ni'hao", "{tag}: 层归属必须沿用原精确条目");
        assert_eq!(hits[0].freq, 900001, "{tag}: 同文本以用户词频为准");
    }
}

/// 用户词本身就是精确命中（新文本）：进层一，且压住层二同文本的词库补全词。
#[test]
fn user_word_with_exact_hit_enters_layer_one() {
    let seed = "...\n来迟\tni hao ci\t500000\n";
    for with_bin in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let tag = if with_bin { "FST" } else { "SQLite" };
        let mut d = dict(dir.path(), seed, with_bin);
        // 全新读音的学习：词库里「来迟」只有长 key `ni'hao'ci`（补全侧）。
        d.learn(&["ni".into(), "hao".into()], "来迟").unwrap();

        let hits = d.lookup_prefix(&["ni".into()], "hao", 10).unwrap();
        assert_eq!(
            texts(&hits),
            vec!["来迟"],
            "{tag}: 精确命中的用户词应独占层一，实际 {hits:?}"
        );
        assert_eq!(hits[0].pinyin, "ni'hao", "{tag}: 用户词进了错误的层");
    }
}
