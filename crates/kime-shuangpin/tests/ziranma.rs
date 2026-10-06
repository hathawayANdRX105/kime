//! 自然码方案行为测试，经 kime_shuangpin::Table 公开 API。

use kime_shuangpin::ziranma::{decode, encode, TABLE};
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

#[test]
fn decode_unique() {
    for i in 0..TABLE.len() {
        for j in (i + 1)..TABLE.len() {
            assert_ne!(TABLE[i].1, TABLE[j].1, "{} and {}", TABLE[i].0, TABLE[j].0);
        }
    }
}

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
