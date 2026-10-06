//! 自然码方案行为测试，经 kime_shuangpin::Table 公开 API。

use kime_shuangpin::{Scheme, Table};

/// 自然码里 "hao" 的末键是 k（不是 c），所以 "nihao" = "nihk"。
/// ponytail: 任务原例 `nihk→[ni,hao]` 实际上配的是自然码，小鹤是 "nihc"。
#[test]
fn nihao_ziranma() {
    let table = Table::new(Scheme::Ziranma);
    assert_eq!(table.to_syllables("nihk").unwrap(), vec!["ni", "hao"]);
}

#[test]
fn odd_length_err() {
    let table = Table::new(Scheme::Ziranma);
    // 单字符 / 3 字符：非偶数 → Err；空串 0%2=0 视为「无输入」不报错。
    assert!(table.to_syllables("n").is_err());
    assert!(table.to_syllables("nih").is_err());
}

#[test]
fn invalid_key_err() {
    let table = Table::new(Scheme::Ziranma);
    // "ab" 不在表里
    assert!(table.to_syllables("ab").is_err());
    // 大写不允许
    assert!(table.to_syllables("Ni").is_err());
}
