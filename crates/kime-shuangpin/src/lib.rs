//! 双拼 → 音节码表。只做进 engine 前的「翻译」，查询管线与全拼共用一套。
//!
//! 契约：双拼固定两键一组（声母键 + 韵母键）；零声母音节按各表规则补位。
//! 码表数据硬编码 const（小鹤/自然码是稳定事实，无需运行时配置）。

use kime_pinyin::Reading;

mod xiaohe;
mod ziranma;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
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

    /// 半截键（只敲了键对的前一半）代表的拼音前缀。
    ///
    /// 从码表推导，不另立一张映射表：以 `u` 为声母键的音节全是 sh 系
    /// （shi/shen/shang/…），所以 `u` 就代表 `"sh"`；`b` 打头全是 b 系，故 `"b"`。
    /// 好处是码表一改这里自动跟着改，不会和码表漂移。
    pub fn initial_of(&self, key: char) -> String {
        if !key.is_ascii_lowercase() {
            return String::new();
        }
        let first = key as u8;
        let mut common: Option<&str> = None;
        for second in b'a'..=b'z' {
            let Some(syl) = (self.decode)(first, second) else {
                continue;
            };
            common = Some(match common {
                None => syl,
                Some(prev) => common_prefix(prev, syl),
            });
            if common == Some("") {
                break; // 已经缩到空，不可能更长
            }
        }
        common.unwrap_or("").to_string()
    }
}

/// ASCII 公共前缀（拼音全 ASCII，按字节切安全）。
fn common_prefix<'a>(a: &'a str, b: &str) -> &'a str {
    let n = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    &a[..n]
}
