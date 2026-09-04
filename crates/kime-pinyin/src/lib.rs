//! 拼音音节切分 — 纯逻辑，零依赖。
//!
//! 契约：ascii 字母串 → 所有合法切分，最优在前。
//! "xian" → [["xian"], ["xi","an"]]；"nihao" → [["ni","hao"]]；空串/非法组合 → 空 vec。
//! 只认 ~410 个合法音节；声母缩写（nh）由 dict 层的 abbrev 列承担，不在此处。

/// 一次切分：音节序列，如 ["ni","hao"]
pub type Reading = Vec<String>;

/// 全部合法切分，最优排序（最长优先；后续可接词频回填）
pub fn segment(input: &str) -> Vec<Reading> {
    let _ = input;
    todo!("M1: 音节表 + DP 切分")
}
