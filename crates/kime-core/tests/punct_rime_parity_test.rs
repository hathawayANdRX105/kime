//! rime-ice 标点表全量对齐测试（用户第 2 条：「中文的字符你还是没做全」）。
//!
//! 期望值 = https://github.com/iDvel/rime-ice default.yaml
//! `punctuator/half_shape`（config_version 2026-02-06）的 32 个条目，逐条钉死：
//! 少一条、错一值、或私自加了 rime 没有的映射，本测试即红。
//! 成对引号 `"` `'` 在 rime 里是 `pair: [开, 闭]`，`map_punct` 存**开引号**，
//! 闭引号由引擎状态机决定（行为契约见 quote_pair_test.rs，这里只钉表值）。

use kime_core::punct::map_punct;

/// rime-ice default.yaml punctuator/half_shape，32 条目（含映射为自身的 10 个半角符号）。
const HALF_SHAPE: &[(char, &str)] = &[
    (',', "，"),
    ('.', "。"),
    ('<', "《"),
    ('>', "》"),
    ('/', "/"),
    ('?', "？"),
    (';', "；"),
    (':', "："),
    ('\'', "‘"),
    ('"', "“"),
    ('\\', "、"),
    ('|', "|"),
    ('`', "·"),
    ('~', "~"),
    ('!', "！"),
    ('@', "@"),
    ('#', "#"),
    ('%', "%"),
    ('$', "¥"),
    ('^', "……"),
    ('&', "&"),
    ('*', "*"),
    ('(', "（"),
    (')', "）"),
    ('-', "-"),
    ('_', "——"),
    ('+', "+"),
    ('=', "="),
    ('[', "【"),
    (']', "】"),
    ('{', "「"),
    ('}', "」"),
];

#[test]
fn rime_half_shape_entries_exact() {
    assert_eq!(HALF_SHAPE.len(), 32, "rime-ice half_shape 应有 32 条目");
    for &(ascii, want) in HALF_SHAPE {
        assert_eq!(
            map_punct(ascii),
            Some(want),
            "half_shape 条目 '{ascii}' 映射错误"
        );
    }
}

#[test]
fn no_entries_outside_rime_table() {
    // 定义域 = 全部可打印 ASCII：表外字符（字母、数字、表未涵盖者）必须 None，
    // 否则引擎会无差别吞键。与上一个测试合起来 = 表与 rime 严格相等。
    let table: std::collections::HashSet<char> = HALF_SHAPE.iter().map(|e| e.0).collect();
    for code in 0x21u8..=0x7e {
        let c = code as char;
        if !table.contains(&c) {
            assert_eq!(map_punct(c), None, "'{c}' 不在 rime half_shape，不应有映射");
        }
    }
}
