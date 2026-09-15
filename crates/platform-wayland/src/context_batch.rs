//! input-method-v2 done 事件的批处理状态机（纯函数，零平台依赖）。
//!
//! 协议规定 surrounding_text / text_change_cause / content_type 只改 pending
//! 状态，真正的生效在 done——文本输入状态在 input-method 上下文里是双缓冲的。
//! 壳层在每个事件暂存、在 done 调 [`plan_context_commit`] 归一出对引擎的一次
//! 动作，让这条链可以被 tests/context_batch_test.rs 用假数据钉住
//! （同族前车之鉴：route.rs——壳层决策没单测，正是真机行为飘的原因）。

use kime_core::context::{is_from_input_method, ContextTail};

/// done 提交后壳层要对引擎执行的动作（纯决策，不含引擎调用本身）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextCommit {
    /// 本批没有 surrounding_text 事件。done 也会因 content_type 等单独到达，
    /// 合成器没说光标位置变了，壳层就不能自作主张清掉旧尾巴——上下文保持不变。
    Unchanged,
    /// 回声（cause == INPUT_METHOD）：状态字段照存，但不推引擎，
    /// 否则输入法把自己的上屏当作用户输入，构成自激循环。
    Echo,
    /// 提交光标前的上下文尾巴；`None` = 光标前无可用上下文
    /// （cursor 劈开 UTF-8 字符被拒收，或光标紧贴句首）。
    Apply(Option<String>),
}

/// done 事件的裁决：把一批暂存状态归一成对引擎的一次动作。
///
/// - `surrounding` = 自上次 done 以来最后一次 `surrounding_text(text, cursor)`，
///   cursor 为**字节**偏移；`None` = 本批没收到该事件。
/// - `cause` = 同批 `text_change_cause`，缺失时协议默认 0（INPUT_METHOD）。
/// - `max_chars` = 尾巴字符上限（壳层传 [`CONTEXT_TAIL_CHARS`]）。
pub fn plan_context_commit(
    surrounding: Option<&(String, usize)>,
    cause: u32,
    max_chars: usize,
) -> ContextCommit {
    // 回声优先裁决：自己的上屏一律不推引擎（字段暂存在事件处理处已完成）。
    if is_from_input_method(cause) {
        return ContextCommit::Echo;
    }
    let Some((text, cursor)) = surrounding else {
        return ContextCommit::Unchanged;
    };
    // 归一失败（cursor 越界或劈开多字节字符）→ 明确的「无可用上下文」，
    // 覆盖旧尾巴；留着过期上下文会把候选带偏。
    let Some(tail) = ContextTail::from_surrounding(text, *cursor) else {
        return ContextCommit::Apply(None);
    };
    ContextCommit::Apply(tail.tail_before_cursor(max_chars).map(str::to_string))
}

/// 上下文尾巴的字符上限：32 个字符足够覆盖一个短语/半句话，
/// 再长也只是给 LLM/分词喂噪声（且每次 done 都要 clone 一份）。
pub const CONTEXT_TAIL_CHARS: usize = 32;
