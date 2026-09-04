//! 双拼 → 音节码表。只做进 engine 前的「翻译」，查询管线与全拼共用一套。
//!
//! 契约：双拼固定两键一组（声母键 + 韵母键）；零声母音节按各表规则补位。
//! 码表数据硬编码 const（小鹤/自然码是稳定事实，无需运行时配置）。

use kime_pinyin::Reading;

mod xiaohe;
mod ziranma;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Xiaohe,
    Ziranma,
}

pub struct Table {
    decode: fn(u8, u8) -> Option<&'static str>,
}

impl Table {
    pub fn new(scheme: Scheme) -> Self {
        let decode = match scheme {
            Scheme::Xiaohe => xiaohe::decode,
            Scheme::Ziranma => ziranma::decode,
        };
        Self { decode }
    }

    /// 双拼按键串 → 音节序列（小鹤 `"nihc"` → ["ni","hao"]；自然码 `"nihk"` → ["ni","hao"]）。
    /// 长度非偶数 / 组合非法 → Err，engine 据此拦截该键。
    pub fn to_syllables(&self, keys: &str) -> Result<Reading, String> {
        if keys.len() % 2 != 0 {
            return Err(format!("odd key length: {}", keys.len()));
        }
        let bytes = keys.as_bytes();
        if !bytes.iter().all(|b| b.is_ascii_lowercase()) {
            return Err("non a-z key".into());
        }
        let mut out = Reading::with_capacity(keys.len() / 2);
        for pair in bytes.chunks_exact(2) {
            match (self.decode)(pair[0], pair[1]) {
                Some(s) => out.push(s.to_string()),
                None => {
                    return Err(format!(
                        "bad key pair: {}{}",
                        pair[0] as char, pair[1] as char
                    ))
                }
            }
        }
        Ok(out)
    }
}
