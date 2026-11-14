//! 音节表单一事实源守门（issue #118）钉的两面：
//!
//! 1. unknown_syllables（纯函数面）：`'` 连接的读音串里逐段查表，不在 416 音节表内
//!    的音节全部返回（空段跳过）、去重保首次出现序，返回项借用输入串；
//! 2. dict import 守门（集成面）：读音含表外音节的条目照常入库并可被查询命中——
//!    stderr 告警是一次性、不阻塞的（仓惯例：stderr 面不钉），本测试钉「不阻塞入库」。
//!
//! 示例音节：`ni`/`hao` 在表内；`sei`/`fong` 是表外罕见音节（rime-luna-pinyin 424
//! 集独有、词库无字，见 src 表头注释）。

use std::fs;

use kime_core::dict::Dict;
use kime_pinyin::unknown_syllables;

fn tmp_db_path(suffix: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir();
    dir.join(format!(
        "kime_import_guard_{}_{}_{}.sqlite",
        suffix,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn unknown_syllables_dedupes_and_keeps_first_occurrence_order() {
    assert_eq!(unknown_syllables("ni'hao"), Vec::<&str>::new());
    assert_eq!(unknown_syllables("ni'sei"), vec!["sei"]);
    assert_eq!(unknown_syllables(""), Vec::<&str>::new());
    assert_eq!(
        unknown_syllables("sei'sei"),
        vec!["sei"],
        "去重保首次出现序"
    );
    assert_eq!(unknown_syllables("sei'fong"), vec!["sei", "fong"]);
}

#[test]
fn import_keeps_gap_rows_in_dict_so_the_warning_never_blocks() {
    let path = tmp_db_path("syllable_guard");
    let _ = fs::remove_file(&path);
    let mut d = Dict::open(&path).expect("open temp db");

    let yaml = std::env::temp_dir().join(format!(
        "kime_import_guard_yaml_{}.yaml",
        std::process::id()
    ));
    // rime-ice 源 yaml 的 pinyin 列是空格分隔音节（import 逐行 split_whitespace
    // 再 join 成 '）。而 ni'hao 里的 ' 会被 import 的 ascii 过滤（dict.rs:743）
    // 整行丢弃——对照行必须写空格分隔的 ni hao。
    fs::write(
        &yaml,
        "---\nname: guardtest\nsort: by_weight\n...\n你好\tni hao\t1000\n甲\tsei\t1\n",
    )
    .unwrap();

    let n = d.import(&yaml).expect("import 不阻塞：缺口行照旧入库");
    assert_eq!(
        n, 2,
        "两行都入库（import 计数 = 本次新增行数，幂等重导返回 0）"
    );

    // 表内读音的对照组（lookup 是整读法精确匹配，「你好」= ni+hao 两音节）：
    let hits = d.lookup(&["ni".into(), "hao".into()], 10).unwrap();
    assert!(
        hits.iter().any(|c| c.text == "你好"),
        "ni+hao 整读法查应命中 你好，实际 {:?}",
        hits.iter().map(|c| c.text.as_str()).collect::<Vec<_>>()
    );
    // 缺口行 sei（表外音节）入库且整读法可查到的可观察面：
    let hits = d.lookup(&["sei".into()], 10).unwrap();
    assert!(
        hits.iter().any(|c| c.text == "甲"),
        "sei 查应命中 甲，实际 {:?}",
        hits.iter().map(|c| c.text.as_str()).collect::<Vec<_>>()
    );

    let _ = fs::remove_file(&path);
    let _ = fs::remove_file(&yaml);
}
