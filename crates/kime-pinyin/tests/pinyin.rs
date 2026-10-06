//! kime-pinyin 单元测试：音节切分契约 + 音节表完整性。

use kime_pinyin::{segment, Reading, SYLLABLES};

/// "xian" → [["xian"], ["xi","an"]]：单音节解析排前 = 最优。
#[test]
fn xian_ambiguous() {
    assert_eq!(
        segment("xian"),
        vec![
            vec!["xian".to_string()],
            vec!["xi".to_string(), "an".to_string()]
        ]
    );
}

/// "nihao"：唯一常用切分是 ni+hao；也存在 ni+ha+o（含 o 单字）。
/// 切分按音节数从少到多排。
#[test]
fn nihao_unique() {
    let r = segment("nihao");
    let want = vec![
        vec!["ni".to_string(), "hao".to_string()],
        vec!["ni".to_string(), "ha".to_string(), "o".to_string()],
    ];
    assert_eq!(r, want);
}

/// "fangan" 与 "fan'gan" 在 ASCII 切分里都合法：fang+an、fan+gan 两个 split。
#[test]
fn fangan_ambiguous() {
    let r = segment("fangan");
    let want = vec![
        vec!["fang".to_string(), "an".to_string()],
        vec!["fan".to_string(), "gan".to_string()],
    ];
    assert_eq!(r, want);
}

/// 单音节：全表任一 1-6 字母音节自身都能被切出；某些也存在更短的二切。
#[test]
fn single_syllable() {
    assert_eq!(segment("zhong"), vec![vec!["zhong".to_string()]]);
    assert_eq!(segment("a"), vec![vec!["a".to_string()]]);
    // "chuang" 也可切 "chu"+"ang"
    assert_eq!(
        segment("chuang"),
        vec![
            vec!["chuang".to_string()],
            vec!["chu".to_string(), "ang".to_string()],
        ]
    );
}

/// 多音节直串：wo+ai+ni 唯一解（"i"/"in" 都不是合法单字音节）。
#[test]
fn multi_syllable() {
    assert_eq!(
        segment("woaini"),
        vec![vec!["wo".to_string(), "ai".to_string(), "ni".to_string()]]
    );
}

/// 非法字符（含大写/数字/中文/标点）一律拒绝。
#[test]
fn invalid_chars() {
    assert!(segment("niHao").is_empty());
    assert!(segment("n1hao").is_empty());
    assert!(segment("你好").is_empty());
    assert!(segment("ni hao").is_empty());
    assert!(segment("ni-hao").is_empty());
}

/// 空串 → 空 vec。
#[test]
fn empty_input() {
    let r: Vec<Reading> = segment("");
    assert!(r.is_empty());
}

/// "xianan"：四向歧义（xian+an / xia+nan / xi+an+an / xi+a+nan）。
/// 期望顺序：2 音节切分（音节数最少）排前，同长度按字典序。
#[test]
fn xianan_3way() {
    let r = segment("xianan");
    let want = vec![
        vec!["xian".to_string(), "an".to_string()],
        vec!["xia".to_string(), "nan".to_string()],
        vec!["xi".to_string(), "an".to_string(), "an".to_string()],
        vec!["xi".to_string(), "a".to_string(), "nan".to_string()],
    ];
    assert_eq!(r, want);
}

/// 全表完整性 sanity：416 条，全部 1-6 字母、小写、无重复。
/// 注：含 a/o/e 三个单字母音节（"啊"/"哦"/"鹅"），所以下界是 1 而非 2。
#[test]
fn table_sanity() {
    use std::collections::HashSet;
    assert_eq!(
        SYLLABLES.len(),
        416,
        "音节表规模漂移：改表请同步 tests/pinyin.rs::table_sanity 的期望集"
    );
    let mut seen = HashSet::new();
    for s in SYLLABLES {
        assert!(!s.is_empty() && s.len() <= 6, "bad length: {s:?}");
        assert!(
            s.bytes().all(|b| b.is_ascii_lowercase()),
            "non-lowercase: {s:?}"
        );
        assert!(seen.insert(*s), "duplicate: {s:?}");
    }
}
