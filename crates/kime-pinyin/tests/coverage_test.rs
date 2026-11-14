//! 全拼音节表全覆盖：src 音节表本体的每条读音，segment() 都必须能作为单音节切出。
//!
//! 被测对象即 src 表本体（单一事实源）；词库覆盖性由 dict import 守门
//! （unknown_syllables）承担——不再有冻结副本可漂移。

use kime_pinyin::{segment, SYLLABLES};

#[test]
fn every_syllable_segments_as_itself() {
    assert_eq!(
        SYLLABLES.len(),
        416,
        "音节表规模须与 src 一致（table_sanity 钉住另一侧）"
    );
    for &s in SYLLABLES {
        let r = segment(s);
        assert!(
            r.iter()
                .any(|reading| reading.len() == 1 && reading[0] == s),
            "segment({s:?}) 切不出自身: {r:?}"
        );
    }
}

#[test]
fn rare_readings_recovered() {
    // 补录前切不出自身、或只能错误拆碎的在册读音
    assert_eq!(segment("ya"), vec![vec!["ya".to_string()]]);
    assert_eq!(segment("yo"), vec![vec!["yo".to_string()]]);
    assert_eq!(segment("kei"), vec![vec!["kei".to_string()]]);
}
