//! 中文标点符号映射表：英文输入的 ASCII 标点 → 全角中文标点。
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
        m.insert('~', "～");
        m
    });
    &*MAP
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn punct_map_full_coverage() {
        let cases: &[char] = &[',', '.', '?', '!', ':', ';', '\\', '_', '^', '(', ')', '<', '>', '[', ']', '~'];
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
}