//! lattice Viterbi 句级联想集成测试。

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::dict::Dict;
use kime_core::lattice::viterbi_sentences;

fn tmp_db(suffix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_lattice_test_{}_{}_{}.sqlite",
        suffix,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_file(&path);
    path
}

fn create_test_dict() -> (Dict, std::path::PathBuf) {
    let db = tmp_db("dict");
    let mut dict = Dict::open(&db).expect("open dict");
    // 使用 import 导入测试数据
    let yaml = r#"
...
你好	ni hao	1000
世界	shi jie	800
我	wo	5000
们	men	3000
爱	ai	2000
你好世界	ni hao shi jie	500
我爱你	wo ai ni	600
"#;
    let yaml_path = std::env::temp_dir().join(format!(
        "kime_lattice_yaml_{}_{}.yaml",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::write(&yaml_path, yaml).unwrap();
    let _ = dict.import(&yaml_path);
    let _ = fs::remove_file(&yaml_path);
    (dict, db)
}
#[test]
fn test_viterbi_two_words() {
    let (dict, db) = create_test_dict();
    // "ni'hao" + "shi'jie" -> "你好世界"
    let reading = vec![
        "ni".to_string(),
        "hao".to_string(),
        "shi".to_string(),
        "jie".to_string(),
    ];
    let cands = viterbi_sentences(&dict, &reading);
    assert_eq!(cands.first().map(|c| c.text.as_str()), Some("你好世界"));
    let _ = fs::remove_file(&db);
}

#[test]
fn test_viterbi_three_words() {
    let (dict, db) = create_test_dict();
    // "wo" + "ai" + "ni" -> "我爱你"
    let reading = vec!["wo".to_string(), "ai".to_string(), "ni".to_string()];
    let cands = viterbi_sentences(&dict, &reading);
    assert_eq!(cands.first().map(|c| c.text.as_str()), Some("我爱你"));
    let _ = fs::remove_file(&db);
}

#[test]
fn test_viterbi_single_syllable_returns_none() {
    let (dict, db) = create_test_dict();
    let reading = vec!["ni".to_string()];
    assert!(viterbi_sentences(&dict, &reading).is_empty());
    let _ = fs::remove_file(&db);
}

#[test]
fn test_viterbi_no_match_fallback() {
    let (dict, db) = create_test_dict();
    // 完全不在词库中的音节
    let reading = vec!["xxx".to_string(), "yyy".to_string()];
    assert!(viterbi_sentences(&dict, &reading).is_empty());
    let _ = fs::remove_file(&db);
}

#[test]
fn test_viterbi_partial_match() {
    let (dict, db) = create_test_dict();
    // "ni'hao" 在库里，"xxx" 不在
    let reading = vec!["ni".to_string(), "hao".to_string(), "xxx".to_string()];
    // 整句无法完整匹配，应返回空（当前实现要求全覆盖）
    assert!(viterbi_sentences(&dict, &reading).is_empty());
    let _ = fs::remove_file(&db);
}
