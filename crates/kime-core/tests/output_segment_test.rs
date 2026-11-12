//! Committed-output dictionary segmentation behavior.

use kime_core::dict::Dict;
use kime_core::lm;
use std::fs;

fn tmp_db(suffix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_output_segment_{}_{}_{}.sqlite3",
        suffix,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_file(&path);
    path
}

fn seed_phrase(d: &Dict, pinyin: &str, text: &str, freq: i64) {
    d.conn()
        .execute(
            "INSERT INTO phrase(pinyin, text, freq, abbrev, user)
             VALUES (?1, ?2, ?3, ?4, 0)",
            rusqlite::params![pinyin, text, freq, ""],
        )
        .unwrap();
}

/// Multiple CJK runs are split at punctuation and Latin text.
#[test]
fn committed_text_splits_multiple_runs() {
    let db = tmp_db("multi");
    let d = Dict::open(&db).unwrap();
    seed_phrase(&d, "ni'hao", "你好", 100);
    seed_phrase(&d, "shi'jie", "世界", 200);
    seed_phrase(&d, "tian'qi", "天气", 150);
    seed_phrase(&d, "zhen", "真", 80);
    seed_phrase(&d, "hao", "好", 90);

    let groups = lm::segment_output_text(d.conn(), "你好,world世界。天气真好").unwrap();
    assert_eq!(
        groups,
        vec![
            vec!["你好".to_string()],
            vec!["世界".to_string()],
            vec!["天气".to_string(), "真".to_string(), "好".to_string()],
        ],
        "multiple CJK runs should be segmented independently"
    );
    let _ = fs::remove_file(&db);
}

/// An exact phrase remains one output token even when child words score higher.
#[test]
fn committed_text_prefers_whole_phrase() {
    let db = tmp_db("whole");
    let d = Dict::open(&db).unwrap();
    seed_phrase(&d, "tian'qi", "天气", 10);
    seed_phrase(&d, "tian", "天", 100);
    seed_phrase(&d, "qi", "气", 100);

    let groups = lm::segment_output_text(d.conn(), "天气").unwrap();
    assert_eq!(groups, vec![vec!["天气".to_string()]]);
    let _ = fs::remove_file(&db);
}

/// An unknown CJK character makes the whole committed run ineligible.
#[test]
fn committed_text_skips_unsegmentable_run() {
    let db = tmp_db("unsegmentable");
    let d = Dict::open(&db).unwrap();
    seed_phrase(&d, "ni'hao", "你好", 100);

    let groups = lm::segment_output_text(d.conn(), "你好抽").unwrap();
    assert!(
        groups.is_empty(),
        "unknown CJK text must not be partially learned"
    );
    let _ = fs::remove_file(&db);
}

/// Non-CJK output has no learnable segmentation.
#[test]
fn committed_text_without_cjk_is_empty() {
    let db = tmp_db("ascii");
    let d = Dict::open(&db).unwrap();
    assert!(lm::segment_output_text(d.conn(), "hello, world!")
        .unwrap()
        .is_empty());
    assert!(lm::segment_output_text(d.conn(), "").unwrap().is_empty());
    assert!(lm::segment_output_text(d.conn(), "12345")
        .unwrap()
        .is_empty());
    let _ = fs::remove_file(&db);
}
