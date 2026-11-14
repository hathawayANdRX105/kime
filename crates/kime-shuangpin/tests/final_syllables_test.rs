//! `Table::final_syllables_of`：双拼奇数键时，该键作**韵母位（第二键）**的音节表。
//!
//! 断言值取自两套码表的 `TABLE`（首键 `a`..`z` 遍历、去重），而非手抄估算。

use kime_shuangpin::{Scheme, Table};

fn final_syllables(scheme: Scheme, key: char) -> Vec<String> {
    Table::new(scheme)
        .final_syllables_of(key)
        .into_iter()
        .map(str::to_string)
        .collect()
}

#[test]
fn ziranma_p_as_final_is_un_un_series() {
    let out = final_syllables(Scheme::Ziranma, 'p');

    assert!(
        out.contains(&"lun".to_string()),
        "自然码 p 作韵母位应含 lun，实际 {out:?}"
    );
    assert!(out.contains(&"jun".to_string()), "…jun");
    assert!(out.contains(&"chun".to_string()), "…chun");
    assert!(out.contains(&"run".to_string()), "…run");

    // 作声母位的音节（pa/pai/ping 族）不该出现在韵母位表里。
    assert!(
        !out.contains(&"pa".to_string()),
        "pa 是声母位音节，不属韵母位表"
    );
    assert!(!out.contains(&"pai".to_string()));
    assert!(!out.contains(&"ping".to_string()));
}

#[test]
fn xiaohe_p_as_final_is_ie_series() {
    let out = final_syllables(Scheme::Xiaohe, 'p');

    assert!(
        out.contains(&"xie".to_string()),
        "小鹤 p 作韵母位应含 xie，实际 {out:?}"
    );
    assert!(out.contains(&"jie".to_string()), "…jie");
    assert!(out.contains(&"pie".to_string()), "…pie");

    assert!(!out.contains(&"pa".to_string()));
    assert!(!out.contains(&"pan".to_string()));
}

#[test]
fn initial_of_semantics_unchanged() {
    let ziranma = Table::new(Scheme::Ziranma);
    assert_eq!(ziranma.initial_of('u'), "sh");
    assert_eq!(ziranma.initial_of('i'), "ch");
    assert_eq!(ziranma.initial_of('v'), "zh");
    // 非 a-z 照旧返回空串。
    assert_eq!(ziranma.initial_of(' '), "");
    assert_eq!(ziranma.initial_of('P'), "");
}

#[test]
fn final_syllables_of_rejects_non_letter() {
    let ziranma = Table::new(Scheme::Ziranma);
    let xiaohe = Table::new(Scheme::Xiaohe);
    assert!(ziranma.final_syllables_of(' ').is_empty(), "空格应得空表");
    assert!(ziranma.final_syllables_of('P').is_empty(), "大写应得空表");
    assert!(ziranma.final_syllables_of('5').is_empty(), "数字应得空表");
    assert!(xiaohe.final_syllables_of(' ').is_empty());
}

#[test]
fn final_syllables_of_is_deterministic_and_ordered_by_first_key() {
    let ziranma = Table::new(Scheme::Ziranma);
    let first = ziranma.final_syllables_of('p');
    let again = ziranma.final_syllables_of('p');
    assert_eq!(first, again, "连续两次调用结果必须相同");

    // 首键按 a..z 遍历：自然码表里首个命中是首键 c 的 cun（a/b 无以 p 为末键的音节）。
    assert_eq!(
        first.first(),
        Some(&"cun"),
        "首元素应按首键顺序排列，实际 {first:?}"
    );

    // 同一顺序下 lun（首键 l）应排在 run（首键 r）之前。
    let lun = first
        .iter()
        .position(|s| *s == "lun")
        .expect("lun 应在表里");
    let run = first
        .iter()
        .position(|s| *s == "run")
        .expect("run 应在表里");
    assert!(lun < run, "lun 应排在 run 之前，实际 {first:?}");
}
