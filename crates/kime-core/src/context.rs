//! 上下文捕获（纯函数，零平台依赖）。
//!
//! input-method-v2 的 `surrounding_text(text, cursor, anchor)` 在这里被
//! 归一成光标前的「上下文尾巴」：LLM 分词 / 离线分词 / 排序只用它，
//! wayland 壳只负责把协议事件喂进 [`ContextTail::from_surrounding`]。
//!
//! 协议声明 cursor 是字节偏移且落在 UTF-8 字符边界；本模块仍防御性校验，
//! 非法输入一律 [`Option::None`] 拒收，绝不 panic。

/// 光标前的输入框上下文（`surrounding_text` 的归一化形式）。
///
/// `cursor` 为字节偏移；`text[cursor..]` 是光标后的内容（本模块不需要）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextTail {
    text: String,
    cursor: usize,
}

impl ContextTail {
    /// 由 `surrounding_text(text, cursor)` 构造。
    ///
    /// cursor 必须 `<= text.len()` 且落在字符边界，否则 `None`
    /// （组合中间的半个字符不存在，截断会产 UTF-8 碎片）。
    pub fn from_surrounding(text: &str, cursor: usize) -> Option<Self> {
        if cursor <= text.len() && text.is_char_boundary(cursor) {
            Some(Self {
                text: text.to_string(),
                cursor,
            })
        } else {
            None
        }
    }

    /// 原文（含光标后部分），诊断用。
    pub fn text(&self) -> &str {
        &self.text
    }

    /// 光标字节偏移。
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// 光标前最多 `max_chars` 个字符的尾巴，**按字符边界截断**。
    ///
    /// - `max_chars == 0` → `None`（调用方用「不要上下文」表达）
    /// - 光标前为空（cursor == 0）→ `None`
    /// - `max_chars` 超过可用字符数 → 返回全部光标前内容
    pub fn tail_before_cursor(&self, max_chars: usize) -> Option<&str> {
        if max_chars == 0 {
            return None;
        }
        let prefix = &self.text[..self.cursor];
        // 从右往左数 max_chars 个字符的起点：char_indices 天然按字符边界切。
        let start = prefix
            .char_indices()
            .rev()
            .nth(max_chars - 1)
            .map(|(i, _)| i)
            .unwrap_or(0);
        let tail = &prefix[start..];
        if tail.is_empty() {
            None
        } else {
            Some(tail)
        }
    }
}

/// input-method-v2 `text_change_cause` 取值：
/// `0` = INPUT_METHOD（本输入法自己上屏的回声），其余为应用侧编辑。
/// 输入法只对回声以外的变化感兴趣（否则会把自己的上屏当作用户输入循环处理）。
pub fn is_from_input_method(cause: u32) -> bool {
    cause == 0
}
