//! M1 dict 集成测试 — 使用 tests/fixtures/test.dict.yaml 作为标准 YAML 输入。

use std::fs;
use std::path::PathBuf;

use kime_core::dict::Dict;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("test.dict.yaml")
}

fn tmp_db_path(suffix: &str) -> PathBuf {
    let dir = std::env::temp_dir();
    dir.join(format!(
        "kime_dict_{}_{}_{}.sqlite",
        suffix,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn dict_open_is_idempotent() {
    let path = tmp_db_path("open_idem");
    let _ = fs::remove_file(&path);

    let mut d1 = Dict::open(&path).expect("first open");
    d1.learn(&["ni".into(), "hao".into()], "你好").unwrap();
    drop(d1);

    // 第二次开同一文件：既不能报 "table already exists"，也必须看得见前一个句柄
    // 写进去的东西 —— 只 expect() 的话，schema 没建全也能算"成功"。
    let d2 = Dict::open(&path).expect("second open");
    let hits = d2.lookup(&["ni".into(), "hao".into()], 10).unwrap();
    assert!(
        hits.iter().any(|c| c.text == "你好"),
        "重开后看不到前一个句柄写入的用户词：实际 {:?}",
        hits.iter().map(|c| c.text.as_str()).collect::<Vec<_>>()
    );

    let _ = fs::remove_file(&path);
}

#[test]
fn dict_import_counts_and_is_idempotent() {
    let path = tmp_db_path("import_count");
    let _ = fs::remove_file(&path);
    let mut d = Dict::open(&path).unwrap();

    let fixture = fixture_path();
    assert!(fixture.exists(), "fixture must exist: {fixture:?}");

    let first = d.import(&fixture).expect("import 1");
    // Fixture has 5 TSV rows after the YAML header.
    assert_eq!(first, 5);

    let second = d.import(&fixture).expect("import 2");
    assert_eq!(second, 0, "re-import must be a no-op");

    let _ = fs::remove_file(&path);
}

#[test]
fn dict_import_skips_yaml_header_and_tolerates_missing_freq() {
    let path = tmp_db_path("import_hdr");
    let _ = fs::remove_file(&path);
    let mut d = Dict::open(&path).unwrap();

    // Build an inline yaml that mimics a real rime .dict.yaml header so we can
    // assert that the YAML front-matter (incl. "...") is fully skipped.
    let dir = std::env::temp_dir();
    let yaml = dir.join(format!(
        "kime_dict_hdr_{}_{}.yaml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::write(
        &yaml,
        "#\u{feff}Rime dictionary\n---\nname: hdrtest\nsort: by_weight\nversion: \"1\"\n...\n世界\tshi jie\t9999\n无频列\twu pin lie\n",
    )
    .unwrap();

    let n = d.import(&yaml).expect("import");
    assert_eq!(n, 2, "header skipped, 2 TSV rows inserted");

    // Row without a frequency column → freq defaults to 0, abbrev still built.
    let hits = d
        .lookup(&["wu".into(), "pin".into(), "lie".into()], 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].freq, 0);

    let _ = fs::remove_file(&path);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn dict_lookup_orders_by_freq_and_respects_limit() {
    let path = tmp_db_path("lookup_order");
    let _ = fs::remove_file(&path);
    let mut d = Dict::open(&path).unwrap();

    let dir = std::env::temp_dir();
    let yaml = dir.join(format!(
        "kime_dict_lookup_{}_{}.yaml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    // Three rows share the pinyin "ha" with different freqs to exercise ordering + limit.
    fs::write(&yaml, "...\nB\tha\t50\nC\tha\t40\nD\tha\t40\n").unwrap();
    d.import(&yaml).expect("import");

    let hits = d.lookup(&["ha".into()], 3).expect("lookup");
    assert_eq!(hits.len(), 3);
    // freq DESC then text ASC: B(50), C(40), D(40)
    assert_eq!(hits[0].text, "B");
    assert_eq!(hits[0].freq, 50);
    assert_eq!(hits[1].text, "C");
    assert_eq!(hits[2].text, "D");
    assert!(!hits[0].ai, "ai must be false for dict results");

    // limit caps the result set.
    let hits2 = d.lookup(&["ha".into()], 2).expect("lookup limit");
    assert_eq!(hits2.len(), 2);
    assert_eq!(hits2[0].text, "B");
    assert_eq!(hits2[1].text, "C");

    let _ = fs::remove_file(&path);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn dict_lookup_uses_fixture_pinyin_join_and_abbrev() {
    let path = tmp_db_path("lookup_fixture");
    let _ = fs::remove_file(&path);
    let mut d = Dict::open(&path).unwrap();

    d.import(fixture_path()).expect("import fixture");

    // "你好" pinyin is "ni hao" → stored as "ni'hao".
    let hits = d.lookup(&["ni".into(), "hao".into()], 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].text, "你好");
    assert_eq!(hits[0].pinyin, "ni'hao");
    assert_eq!(hits[0].freq, 5000);
    assert!(!hits[0].ai);

    let _ = fs::remove_file(&path);
}

#[test]
fn dict_learn_bumps_existing_or_inserts_user_row_and_changes_order() {
    let path = tmp_db_path("learn_bump");
    let _ = fs::remove_file(&path);
    let mut d = Dict::open(&path).unwrap();

    let dir = std::env::temp_dir();
    let yaml = dir.join(format!(
        "kime_dict_learn_{}_{}.yaml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::write(&yaml, "...\n他\tta\t5\n").unwrap();
    d.import(&yaml).expect("import");

    // Learn "他" (already present) → freq bump from 5 → 6.
    d.learn(&["ta".into()], "他").expect("learn existing");
    let h1 = d.lookup(&["ta".into()], 10).unwrap();
    assert_eq!(h1.len(), 1);
    assert_eq!(h1[0].freq, 6);

    // Learn a brand-new user word under "ta"; freq=1 row inserted.
    d.learn(&["ta".into()], "它").expect("learn new");
    let h2 = d.lookup(&["ta".into()], 10).unwrap();
    assert_eq!(h2.len(), 2);
    // freq DESC tie-break by text ASC: "他"(6) < "它"(1).
    assert_eq!(h2[0].text, "他");
    assert_eq!(h2[0].freq, 6);
    assert_eq!(h2[1].text, "它");
    assert_eq!(h2[1].freq, 1);

    let _ = fs::remove_file(&path);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn dict_empty_reading_returns_empty() {
    let path = tmp_db_path("empty_reading");
    let _ = fs::remove_file(&path);
    let d = Dict::open(&path).unwrap();

    let hits = d.lookup(&[], 10).expect("lookup empty");
    assert!(hits.is_empty());

    let _ = fs::remove_file(&path);
}

#[test]
fn dict_lookup_abbrev_matches_prefix_and_orders_by_freq() {
    let path = tmp_db_path("abbrev_prefix");
    let _ = fs::remove_file(&path);
    let mut d = Dict::open(&path).unwrap();

    // Fixture rows: 你好 nh/5000, 世界 sj/9999, 我们 wm/4000, 测试 cs/3000, 无频列 wpl/0.
    d.import(fixture_path()).expect("import fixture");

    // "n" is a prefix of "nh" → only "你好" matches; full-pinyin rows are excluded.
    let hits = d.lookup_abbrev("n", 10).expect("abbrev n");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].freq, 5000);

    // "w" is a prefix of both "wm" (我们) and "wpl" (无频列) → 2 rows ordered freq DESC.
    let hits_w = d.lookup_abbrev("w", 10).expect("abbrev w");
    assert_eq!(hits_w.len(), 2);
    assert_eq!(hits_w[0].text, "我们");
    assert_eq!(hits_w[0].freq, 4000);
    assert_eq!(hits_w[1].text, "无频列");
    assert_eq!(hits_w[1].freq, 0);

    // Exact match "nh" still works.
    let hits_nh = d.lookup_abbrev("nh", 10).expect("abbrev nh");
    assert_eq!(hits_nh.len(), 1);
    assert_eq!(hits_nh[0].text, "你好");

    // No match → empty vec.
    let hits_none = d.lookup_abbrev("z", 10).expect("abbrev z");
    assert!(hits_none.is_empty());

    // limit caps the result.
    let hits_lim = d.lookup_abbrev("w", 1).expect("abbrev w limit");
    assert_eq!(hits_lim.len(), 1);
    assert_eq!(hits_lim[0].text, "我们");

    let _ = fs::remove_file(&path);
}

#[test]
fn dict_lookup_abbrev_does_not_affect_full_pinyin_lookup() {
    let path = tmp_db_path("abbrev_iso");
    let _ = fs::remove_file(&path);
    let mut d = Dict::open(&path).unwrap();
    d.import(fixture_path()).expect("import fixture");

    // abbrev queries are an isolated path; full-pinyin lookup is unchanged.
    let full = d
        .lookup(&["ni".into(), "hao".into()], 10)
        .expect("full lookup");
    assert_eq!(full.len(), 1);
    assert_eq!(full[0].text, "你好");

    // And abbrev("n") returns the same row, confirming cross-path consistency.
    let abbr = d.lookup_abbrev("nh", 10).expect("abbrev nh");
    assert_eq!(abbr.len(), 1);
    assert_eq!(abbr[0].text, "你好");

    let _ = fs::remove_file(&path);
}

#[test]
fn dict_top_user_returns_only_user_rows() {
    let path = tmp_db_path("top_user");
    let _ = fs::remove_file(&path);
    let mut d = Dict::open(&path).unwrap();
    d.import(fixture_path()).expect("import fixture");

    // Before any learn() call, no user rows exist → top_user is empty.
    let empty = d.top_user(10).expect("top_user empty");
    assert!(empty.is_empty());

    // learn() 把词条提升为 user=1（自学习语义）：新插入与已导入 bump 都是 user 行。
    d.learn(&["ta".into()], "它").expect("learn new");
    d.learn(&["ni".into(), "hao".into()], "你好")
        .expect("learn existing");
    d.learn(&["ta".into()], "它").expect("learn bump");

    let user_rows = d.top_user(10).expect("top_user");
    // "它"（新插入）与 "你好"（导入后 learn 提升）都是 user 行
    assert_eq!(user_rows.len(), 2);
    assert_eq!(user_rows[0].text, "你好"); // freq 5000 + 1 bump
    assert_eq!(user_rows[0].freq, 5001);
    assert_eq!(user_rows[1].text, "它");
    assert_eq!(user_rows[1].freq, 2); // bump from learn "它" twice
    assert!(!user_rows[1].ai);

    // limit clamps to the requested count.
    let clamped = d.top_user(0).expect("top_user limit 0");
    assert!(clamped.is_empty());

    let _ = fs::remove_file(&path);
}

#[test]
fn dict_lookup_abbrev_empty_or_non_a_z_returns_empty() {
    let path = tmp_db_path("abbrev_empty");
    let _ = fs::remove_file(&path);
    let mut d = Dict::open(&path).unwrap();
    d.import(fixture_path()).expect("import fixture");

    assert!(d.lookup_abbrev("", 10).expect("abbrev empty").is_empty());
    assert!(d.lookup_abbrev("N", 10).expect("abbrev upper").is_empty());
    assert!(d.lookup_abbrev("n1", 10).expect("abbrev digit").is_empty());
    assert!(d.lookup_abbrev("n h", 10).expect("abbrev space").is_empty());

    let _ = fs::remove_file(&path);
}
