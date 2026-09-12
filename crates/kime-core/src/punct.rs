//! 中文标点符号映射表：英文输入的 ASCII 标点 → 中文标点。
//!
//! **基准 = rime `default.yaml` 的 `punctuator/half_shape`**（rime 中文模式日常用的半角表，
//! `full_shape` 仅全角模式用）——想改「手感」之前先对照那张表，别顺手发明映射。
//! 与 rime 的已知差异只有一处实现选择：`- = + * / @ # % & |` 在 rime 里映射为自身，
//! 这里直接不映射（放行宿主），结果同为 ASCII 上屏。
//! 成对引号（`"` `'`）：这里只给开引号，闭引号由 `Engine::quote_open` 状态机决定——
//! 同一个引号键连按第二次出 `”`/`’`，其它任何键重置（对齐 rime 的 `pair: [开, 闭]`）。
use std::sync::LazyLock;

/// 返回字符对应的全角中文标点，如果不是定义的标点则返回 None。
pub fn map_punct(c: char) -> Option<&'static str> {
    // 使用静态哈希映射，O(1) 查找
    let m = punct_map();
    m.get(&c).copied()
}

fn punct_map() -> &'static std::collections::HashMap<char, &'static str> {
    static MAP: LazyLock<std::collections::HashMap<char, &'static str>> = LazyLock::new(|| {
        let mut m = std::collections::HashMap::new();
        m.insert(',', "，");
        m.insert('.', "。");
        m.insert('?', "？");
        m.insert('!', "！");
        m.insert(':', "：");
        m.insert(';', "；");
        m.insert('\\', "、");
        m.insert('_', "——");
        m.insert('^', "……");
        m.insert('(', "（");
        m.insert(')', "）");
        m.insert('<', "《");
        m.insert('>', "》");
        m.insert('[', "【");
        m.insert(']', "】");
        m.insert('~', "~"); // rime half_shape：半角波浪线，不是 ～
        m.insert('"', "“");
        m.insert('\'', "‘");
        m.insert('$', "¥");
        m.insert('`', "·"); // 间隔号
        m.insert('{', "「");
        m.insert('}', "」");
        m
    });
    &*MAP
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn punct_map_full_coverage() {
        let cases: &[char] = &[
            ',', '.', '?', '!', ':', ';', '\\', '_', '^', '(', ')', '<', '>', '[', ']', '~', '"',
            '\'', '$', '`', '{', '}',
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

    #[test]
    fn punct_map_keeps_ascii_operators_literal() {
        // 中文语境下这些通常就是要原样输入（代码、数字、单位），不该转全角
        // （`$ ` { }` 已按 rime half_shape 补齐映射，见上表）
        for c in ['-', '=', '+', '*', '/', '@', '#', '%', '&', '|'] {
            assert!(map_punct(c).is_none(), "'{}' 不应被映射为全角", c);
        }
    }
}
