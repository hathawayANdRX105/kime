//! 双拼 → 音节码表。只做进 engine 前的「翻译」，查询管线与全拼共用一套。
//!
//! 契约：双拼固定两键一组（声母键 + 韵母键）；零声母音节按各表规则补位。
//! 码表数据硬编码 const（小鹤/自然码是稳定事实，无需运行时配置）。

use kime_pinyin::Reading;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Xiaohe,
    Ziranma,
}

pub struct Table;

impl Table {
    pub fn new(_scheme: Scheme) -> Self {
        todo!("M2")
    }

    /// 双拼按键串 → 音节序列（小鹤 "nihk" → ["ni","hao"]）。
    /// 长度非偶数 / 组合非法 → Err，engine 据此拦截该键。
    pub fn to_syllables(&self, _keys: &str) -> Result<Reading, String> {
        todo!("M2")
    }
}
