//! 中文标点符号映射表：英文输入的 ASCII 标点 → 中文标点。
//!
//! **基准 = rime-ice `default.yaml` 的 `punctuator/half_shape`（2026-02-06 版，32 条目）**——
//! 本表与它逐条对齐、零缺项，包括 rime 映射为「自身」的 10 个半角符号
//! （`/ | @ # % & * - + =`：rime 里同样直接上屏该半角字符，这里显式映射为自身，
//! 由引擎 Commit 而非宿主透传，屏面结果一致）。
//! `half_shape` 的 `_`/`^` 即 `——`/`……`；成对引号（`"` `'`）只存**开引号**，
//! 闭引号由 `Engine::quote_open` 状态机决定（对齐 rime 的 `pair: [开, 闭]`）。
//!
//! **full_shape（全角表）已备好未接入**：rime-ice 的 `punctuator/full_shape` 共 32 条目，
//! 主值见下表（rime 的复选条目 `[a, b, …]` 取第一值，复选语义本表不表达）。
//! 引擎的 `PunctMode` 只有 Chinese/English 两态、无全角开关，接入需先加模式（勿顺手加配置项）。
//! ```text
//! ' 　  , ，  . 。  < 《  > 》  / ／(÷)  ? ？  ; ；  : ：  ' ‘’  " “”  \ 、(＼)
//! | ·(｜§¦)  ` ｀  ~ ～  ! ！  @ ＠(☯)  # ＃(⌘)  % ％(°℃)  $ ￥($€£¥¢¤)  ^ ……
//! & ＆  * ＊(·・×※❂)  ( （  ) ）  - －  _ ——  + ＋  = ＝  [ 「(【〔［)  ] 」(】〕］)
//! { 『(〖｛)  } 』(〗｝)
//! ```
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
        // —— 以下 32 条 = rime-ice default.yaml punctuator/half_shape 全集 ——
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
        // rime half_shape 映射为自身的 10 条（显式补全：屏面仍是半角，但走引擎上屏路径）
        m.insert('/', "/");
        m.insert('|', "|");
        m.insert('@', "@");
        m.insert('#', "#");
        m.insert('%', "%");
        m.insert('&', "&");
        m.insert('*', "*");
        m.insert('-', "-");
        m.insert('+', "+");
        m.insert('=', "=");
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
}
