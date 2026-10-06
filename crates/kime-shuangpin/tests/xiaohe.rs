//! 小鹤方案行为测试，经 kime_shuangpin::Table 公开 API。

use kime_shuangpin::xiaohe::{decode, encode, TABLE};
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

/// 全表 round-trip：每个键（含别名键）解回本音节；encode() 取表内首键（规范码），
/// 对每个音节 编码→解码→编码 必须稳定。
#[test]
fn round_trip_all() {
    for &(syl, key) in TABLE {
        assert_eq!(decode(key[0], key[1]), Some(syl), "decode({key:?})");
    }
    // 同一音节的多行（rime derive 别名）键互不相同，首行为规范码
    let mut seen = std::collections::HashSet::new();
    for &(syl, key) in TABLE {
        if !seen.insert(syl) {
            continue;
        }
        assert_eq!(encode(syl), Some(key), "encode({syl}) 应为表内首键");
        let decoded = decode(key[0], key[1]).unwrap();
        assert_eq!(
            encode(decoded),
            Some(key),
            "encode∘decode({syl}) round-trip"
        );
    }
    assert_eq!(
        seen.len(),
        415,
        "音节数应为 415（425 行 = 415 音节 + 10 别名；lo 与 luo 撞键未收录）"
    );
}

/// 解码唯一性：任何两行不共享同一键对（别名是同一音节的不同键，不违反此断言；
/// 二分查找的正确性依赖键互异）。
#[test]
fn decode_unique() {
    for i in 0..TABLE.len() {
        for j in (i + 1)..TABLE.len() {
            assert_ne!(TABLE[i].1, TABLE[j].1, "{} and {}", TABLE[i].0, TABLE[j].0);
        }
    }
}

/// 解码密度：26×26 全表扫描，命中数应 = TABLE.len()。
#[test]
fn decode_density() {
    let mut hits = 0;
    for a in b'a'..=b'z' {
        for b in b'a'..=b'z' {
            if decode(a, b).is_some() {
                hits += 1;
            }
        }
    }
    assert_eq!(hits, TABLE.len());
}
