//! 回归：主查询把候选列表占满（candidate_limit）时，首音节单字仍应**全量**追加。
//!
//! 之前 `append_first_syllable_candidates` 截断到「剩余空间」
//! （`limit - cands.len()`，floor 1）：主查询占满 50 时只剩 1 个位置，
//! 实测 `zhongguoren` 的首音节单字从几十个塌缩到 1 个「中」。
//! 参照 qingjian/fcitx5（候选总量可远超一页，翻页在平台层），
//! 首音节单字预算独立于主查询的 candidate_limit。

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::engine::{Engine, Key};
use std::fs;
use std::path::PathBuf;

/// 首音节 zhong 放 6 个单字；zhongguo* 放 50 条同音节词条把主查询占满。
/// 全拼切分为 ["zhong","guo","ren"]，3 音节 → 触发降级追加。
const CN: &str = "---\nname: test\n...\n\
中\tzhong\t900000\n\
钟\tzhong\t100\n\
忠\tzhong\t90\n\
众\tzhong\t80\n\
终\tzhong\t70\n\
肿\tzhong\t60\n\
中国人\tzhong guo ren\t9000\n\
中国人民\tzhong guo ren min\t8000\n\
中国人的\tzhong guo ren de\t7000\n\
中国人寿\tzhong guo ren shou\t6000\n\
中国仁\tzhong guo ren\t5000\n\
中介人\tzhong guo ren\t4000\n\
钟国人\tzhong guo ren\t3000\n\
忠国人\tzhong guo ren\t2000\n\
众国人\tzhong guo ren\t1000\n";

fn tmp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "kime_fullfirst_{tag}_{}_{}",
        std::process::id(),
        nanos
    ))
}

fn engine_at(tag: &str) -> (Engine, PathBuf) {
    let dir = tmp_dir(tag);
    fs::create_dir_all(&dir).unwrap();
    let yaml = dir.join("cn.dict.yaml");
    fs::write(&yaml, CN).unwrap();
    let mut dict = Dict::open(dir.join("dict.sqlite3")).unwrap();
    dict.import(&yaml).unwrap();
    let e = Engine::new(dict, Config::default());
    (e, dir)
}

fn k(c: char) -> Key {
    Key {
        ch: Some(c),
        code: 0,
        shift: false,
        ctrl: false,
        alt: false,
    }
}

#[test]
fn first_syllable_chars_survive_full_candidate_list() {
    let (mut e, dir) = engine_at("main");
    for c in "zhongguoren".chars() {
        e.key(k(c));
    }
    let texts: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
    for ch in ["中", "钟", "忠", "众", "终", "肿"] {
        assert!(
            texts.iter().any(|t| t == ch),
            "首音节单字「{ch}」应出现在候选列表，实际 {texts:?}"
        );
    }
    // 落位契约：整句/词条在前，单字在后（「中」不应抢「中国人」的首选位）
    let first = texts.first().expect("候选非空");
    assert!(
        first.chars().count() > 1,
        "首选应仍是整句/词条，实际 {first:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}
