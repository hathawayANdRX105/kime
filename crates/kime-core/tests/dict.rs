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

    let _d1 = Dict::open(&path).expect("first open");
    // Second open over the same file must succeed without "table already exists" errors.
    let _d2 = Dict::open(&path).expect("second open");

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
    let hits = d.lookup(&["wu".into(), "pin".into(), "lie".into()], 10).unwrap();
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
