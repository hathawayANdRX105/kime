//! punct 全角映射表集成测试。

use kime_core::punct::map_punct;

#[test]
fn punct_map_full_coverage() {
    let cases: &[char] = &[
        ',', '.', '?', '!', ':', ';', '\\', '_', '^', '(', ')', '<', '>', '[', ']', '~', '"', '\'',
        '$', '`', '{', '}',
    ];
    for &c in cases {
        let mapped = map_punct(c);
        assert!(mapped.is_some(), "标点 '{}' 缺失映射", c);
        assert!(!mapped.unwrap().is_empty(), "映射为空");
    }
}

#[test]
fn punct_map_non_punct() {
    assert!(map_punct('a').is_none());
    assert!(map_punct('1').is_none());
    assert!(map_punct('😀').is_none());
    assert!(map_punct('\n').is_none());
}
