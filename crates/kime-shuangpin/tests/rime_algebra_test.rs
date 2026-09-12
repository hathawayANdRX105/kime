//! 双拼码表 vs rime 规则的守卫测试。
//!
//! 两份码表（`ziranma.rs` / `xiaohe.rs`）都是把 rime schema 的 `speller/algebra`
//! 规则**手工展开**成静态查表的。手写会错：本轮查出 `yan/yao/ye/you` 四个 y 声母
//! 音节在两套表里都编错了（把 `y` 当介音 `i` 去套 `ian/iao/ie/iu` 的韵母键，
//! 而 rime 的 algebra 不做那个替换）。用户照 rime 打 `yb`(you) 直接无解。
//!
//! fixtures 是机械推导出来的，不是手抄：拿 rime schema 的 algebra 规则跑一遍
//! 合法音节表（`$n` 要换成 `\n` 才喂得进 Python 的 re.sub），保留长度恰为 2 字母的
//! 结果。换 rime 版本或扩充音节表时重新生成 fixture、再修码表，别改断言迁就实现。

use std::collections::HashMap;
use std::fs;

use kime_shuangpin::{Scheme, Table};

/// 读 fixture：音节 → rime 认可的合法码集合。
fn allowed(path: &str) -> Vec<(String, Vec<String>)> {
    let text = fs::read_to_string(path).expect("fixture 缺失");
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let (syl, codes) = l.split_once('\t').expect("fixture 行格式：音节\\t码1,码2");
            (
                syl.to_string(),
                codes.split(',').map(str::to_string).collect(),
            )
        })
        .collect()
}

/// 穷举 676 个键对，拿到这张表实际接受的码 → 音节。
fn decoded(table: &Table) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for a in b'a'..=b'z' {
        for b in b'a'..=b'z' {
            let key = format!("{}{}", a as char, b as char);
            if let Ok(reading) = table.to_syllables(&key) {
                if reading.len() == 1 {
                    map.insert(key, reading[0].clone());
                }
            }
        }
    }
    map
}

fn check(fixture: &str, scheme: Scheme, label: &str) -> usize {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/").to_string() + fixture;
    let rows = allowed(&path);
    let got = decoded(&Table::new(scheme));
    assert_eq!(got.len(), rows.len(), "{label}: 可解码键对数与音节数不符");

    let by_syl: HashMap<&str, &[String]> = rows
        .iter()
        .map(|(s, codes)| (s.as_str(), codes.as_slice()))
        .collect();

    // 方向 1：rime 的合法码里至少有一个被本表接受、且解到该音节。
    // 缺了就是「用户照 rime 打字，我们解不出来」—— you/yan/yao/ye 正是如此。
    for (syl, codes) in &rows {
        let kime_key = got.iter().find(|(_, s)| *s == syl).map(|(k, _)| k.clone());
        assert!(
            codes
                .iter()
                .any(|c| got.get(c).map(String::as_str) == Some(syl.as_str())),
            "{label}: 音节 {syl} 的 rime 合法码 {codes:?} 都解不出来（本表把它编成了 {kime_key:?}）"
        );
    }

    // 方向 2：本表接受的每个码都必须 rime 认、且解到 rime 认定的那个音节。
    // 多了就是凭空发明键位（`yq` 之类），照样让用户打出错字。
    for (key, syl) in &got {
        let ok = by_syl
            .get(syl.as_str())
            .is_some_and(|codes| codes.iter().any(|c| c == key));
        assert!(ok, "{label}: 键位 {key} 解成 {syl}，但 rime 不认这个组合");
    }
    rows.len()
}

#[test]
fn ziranma_table_matches_rime_algebra() {
    // fixture 被清空/截断时 helper 里的双向断言会假过，规模在这里钉住
    assert!(
        check("rime_ziranma.tsv", Scheme::Ziranma, "自然码") >= 400,
        "自然码 fixture 规模异常"
    );
}

#[test]
fn xiaohe_table_matches_rime_algebra() {
    assert!(
        check("rime_xiaohe.tsv", Scheme::Xiaohe, "小鹤") >= 400,
        "小鹤 fixture 规模异常"
    );
}

#[test]
fn pending_keys_expand_to_their_initials() {
    // 半截键查的是声母。两套方案都把 zh/ch/sh 挪到了 v/i/u，其余键即本字母。
    for scheme in [Scheme::Ziranma, Scheme::Xiaohe] {
        let t = Table::new(scheme);
        for (key, want) in [
            ('u', "sh"),
            ('i', "ch"),
            ('v', "zh"),
            ('b', "b"),
            ('j', "j"),
        ] {
            assert_eq!(t.initial_of(key), want, "{scheme:?} 的半截键 {key}");
        }
        assert_eq!(t.initial_of(' '), "", "非 a-z 键不该给出任何前缀");
    }
}

#[test]
fn the_reported_codes_decode() {
    // 用户点名的四个，外加两个本轮实测已正确的对照
    let t = Table::new(Scheme::Ziranma);
    for (keys, want) in [
        ("yb", "you"),
        ("yj", "yan"),
        ("yk", "yao"),
        ("ye", "ye"),
        ("ui", "shi"),
        ("uf", "shen"),
    ] {
        assert_eq!(
            t.to_syllables(keys).ok().map(|r| r.join("")).as_deref(),
            Some(want),
            "自然码 {keys} 应解为 {want}"
        );
    }
}
