//! 小鹤方案行为测试，经 kime_shuangpin::Table 公开 API。

use kime_shuangpin::{Scheme, Table};

/// 任务里给的典型示例：标准小鹤 `nihc` → `["ni","hao"]`。
/// ponytail: 任务原文写成 `nihk`，那是 typo（k 是 ing 不是 ao）。
#[test]
fn nihao_xiaohe() {
    let table = Table::new(Scheme::Xiaohe);
    assert_eq!(table.to_syllables("nihc").unwrap(), vec!["ni", "hao"]);
}

#[test]
fn odd_length_err() {
    let table = Table::new(Scheme::Xiaohe);
    // 单字符 / 3 字符：非偶数 → Err；空串 0%2=0 视为「无输入」不报错。
    assert!(table.to_syllables("n").is_err());
    assert!(table.to_syllables("nih").is_err());
}

#[test]
fn invalid_key_err() {
    let table = Table::new(Scheme::Xiaohe);
    // "ab" 不在表里（a 后面跟 b 不是任何合法音节键对）
    assert!(table.to_syllables("ab").is_err());
    // 大写不允许
    assert!(table.to_syllables("Ni").is_err());
}
