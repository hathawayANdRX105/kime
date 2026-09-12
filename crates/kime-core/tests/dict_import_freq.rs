//! 词库导入频率回归测试。
//!
//! 原缺陷：`Dict::import` 用 `INSERT OR IGNORE`，同一个 (pinyin, text) 冲突时
//! 「先插入者胜」。rime-ice 的 41448 字表没有频率列（freq=0），字典序又排在
//! 8105 之前，于是 8105 的真实频率被 IGNORE 掉 —— 全库 54% 的行 freq=0，
//! 单字在 (freq DESC, text ASC) 排序里被多音节词彻底压住。

use std::fs;
use std::path::PathBuf;

use kime_core::dict::Dict;

fn tmp_path(prefix: &str, ext: &str) -> PathBuf {
    let dir = std::env::temp_dir();
    dir.join(format!(
        "kime_ifreq_{}_{}_{}.{}",
        prefix,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        ext
    ))
}

fn write_yaml(name: &str, body: &str) -> PathBuf {
    let path = tmp_path(name, "yaml");
    fs::write(&path, format!("...\n{body}")).unwrap();
    path
}

/// 精确查 `text` 在 `reading` 下的频率；查不到直接 panic（断言才有意义）。
fn freq_of(d: &Dict, reading: &[&str], text: &str) -> u64 {
    let keys: Vec<String> = reading.iter().map(|s| s.to_string()).collect();
    let hits = d.lookup(&keys, 50).unwrap();
    hits.iter()
        .find(|c| c.text == text)
        .unwrap_or_else(|| panic!("`{text}` 不在 {reading:?} 的候选里: {hits:?}"))
        .freq
}

/// 原 bug 的直接复现：先喂无频率行（41448 风格），再喂有频率行（8105 风格）。
#[test]
fn import_zero_then_weighted_keeps_weighted_freq() {
    let db = tmp_path("zero_first", "sqlite");
    let _ = fs::remove_file(&db);
    let mut d = Dict::open(&db).unwrap();

    let no_freq = write_yaml("no_freq", "是\tshi\n别的\tbie de\n");
    let weighted = write_yaml("weighted", "是\tshi\t31422712\n");

    d.import(&no_freq).expect("import 41448-like");
    d.import(&weighted).expect("import 8105-like");

    assert_eq!(
        freq_of(&d, &["shi"], "是"),
        31422712,
        "后到的真实频率必须覆盖先前的 0，否则单字永远排在多音节词之后"
    );

    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&no_freq);
    let _ = fs::remove_file(&weighted);
}

/// 反向顺序同样成立 → 证明结果对导入顺序不再敏感。
#[test]
fn import_weighted_then_zero_keeps_weighted_freq() {
    let db = tmp_path("weighted_first", "sqlite");
    let _ = fs::remove_file(&db);
    let mut d = Dict::open(&db).unwrap();

    let weighted = write_yaml("weighted", "是\tshi\t31422712\n");
    let no_freq = write_yaml("no_freq", "是\tshi\n");

    d.import(&weighted).expect("import 8105-like");
    d.import(&no_freq).expect("import 41448-like");

    assert_eq!(
        freq_of(&d, &["shi"], "是"),
        31422712,
        "先到的真实频率不能被后到的 0 冲掉"
    );

    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&weighted);
    let _ = fs::remove_file(&no_freq);
}

/// 用户自学习词（user=1）的频率是学习次数，导入绝不能改写它。
#[test]
fn import_never_lowers_user_learned_freq() {
    let db = tmp_path("user", "sqlite");
    let _ = fs::remove_file(&db);
    let mut d = Dict::open(&db).unwrap();

    // 词库里「是/shi」有极高的真实频率。
    let dict_yaml = write_yaml("dict", "是\tshi\t999999\n");
    d.import(&dict_yaml).expect("import dict");

    // 用户学了另一个同音词，freq 从 1 开始累加。
    d.learn(&["shi".into()], "柿").unwrap();
    d.learn(&["shi".into()], "柿").unwrap();
    let before = freq_of(&d, &["shi"], "柿");
    assert_eq!(before, 2, "两条 learn 应累计到 2");

    // 再导入一批词库数据（含同名 pinyin 的其它条目）。
    let more = write_yaml("more", "柿\tshi\t0\n事\tshi\t500\n");
    d.import(&more).expect("import more");

    assert_eq!(
        freq_of(&d, &["shi"], "柿"),
        2,
        "导入不得改动 user=1 行的频率"
    );

    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&dict_yaml);
    let _ = fs::remove_file(&more);
}

/// 幂等性：同一文件重复导入，频率既不会翻倍也不会被冲掉。
#[test]
fn repeated_import_is_idempotent_on_freq() {
    let db = tmp_path("idem", "sqlite");
    let _ = fs::remove_file(&db);
    let mut d = Dict::open(&db).unwrap();

    let yaml = write_yaml("twice", "是\tshi\t31422712\n");
    d.import(&yaml).unwrap();
    d.import(&yaml).unwrap();

    assert_eq!(freq_of(&d, &["shi"], "是"), 31422712);

    let keys: Vec<String> = vec!["shi".into()];
    let hits = d.lookup(&keys, 50).unwrap();
    assert_eq!(
        hits.iter().filter(|c| c.text == "是").count(),
        1,
        "重复导入不能产生重复行"
    );

    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

/// 两类源数据卫生过滤：rime-ice 正文里混着「引擎永远查不到的行」和「被注释掉的示例行」，
/// 都不许落库。
/// - `pinyin = "100"`（tencent.dict.yaml 全表 980,961 条）：组合缓冲只收 a-z
///   （`engine.rs` 的 `is_ascii_alphabetic` 分支），数字键是页内选词 → 永远命中不了。
/// - text 以 `#` 开头（库里 11,726 条，`# 那`/nei 频率高达 9,929,703）：那是 rime-ice
///   注释掉的示例行。频率修好之前它被 0 压着看不见，修好后会直接顶在首候选。
#[test]
fn import_skips_unreachable_and_commented_rows() {
    let db = tmp_path("hygiene", "sqlite");
    let _ = fs::remove_file(&db);
    let mut d = Dict::open(&db).unwrap();

    let yaml = write_yaml(
        "hygiene",
        "一百\t100\t5000\n# 那\tnei\t9929703\n那\tnei\t88\n一\t yi \t7\n",
    );
    let n = d.import(&yaml).expect("import");

    assert_eq!(n, 2, "只该落下 那/yi 两行: {n}");
    let keys: Vec<String> = vec!["100".into()];
    assert!(
        d.lookup(&keys, 10).unwrap().is_empty(),
        "pinyin=100 不该存在"
    );

    // `# 那` 被挡在门外，同音的正常条目「那」不受影响，且修好后的频率照常生效。
    let nei: Vec<String> = vec!["nei".into()];
    let hits = d.lookup(&nei, 10).unwrap();
    assert!(
        !hits.iter().any(|c| c.text.starts_with('#')),
        "注释行混进了候选: {hits:?}"
    );
    assert_eq!(freq_of(&d, &["nei"], "那"), 88);
    assert_eq!(freq_of(&d, &["yi"], "一"), 7);

    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}
