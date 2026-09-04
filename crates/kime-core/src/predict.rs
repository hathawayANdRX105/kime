//! M5: AI 预测 — 异步第二梯队，永不上按键主路（主路 P99 < 20ms 与本模块无关）。
//!
//! 契约：core 不依赖任何 HTTP 栈。壳侧后台任务持有 [`Predictor`]，
//! 触发时机（停顿 ~300ms / 句尾）由壳判断；结果经
//! [`crate::engine::Engine::merge_ai`] 回流；AI 候选被选中走 [`crate::dict::Dict::learn`] 喂频率。

use crate::Candidate;

/// 预测上下文
#[derive(Clone, Debug)]
pub struct Context {
    /// 已上屏文本的句子尾（给模型的语言线索）
    pub history_tail: String,
    /// 当前未上屏内容（拼音串或首选候选）
    pub pending: String,
}

pub struct Predictor {
    /// OpenAI 兼容端点（本地 vLLM / 9router）
    pub endpoint: String,
}

impl Predictor {
    /// 在后台线程 await；返回 ≤ n 条候选
    pub async fn predict(&self, _ctx: &Context, _n: usize) -> Vec<Candidate> {
        todo!("M5")
    }
}
