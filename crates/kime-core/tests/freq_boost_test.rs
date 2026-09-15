//! 高频词调频（方案 B：使用即提升 + 时间衰减）的验收测试。
//!
//! 1. BOOST 校准——第 1 次使用（n=1，加成 300k）压不过 freq=500,000 的普通
//!    语料词；第 2 次（600k）压过（首用即满额，用户拍板 2026-09-15）。
//! 2. 导出频率不变——`Candidate.freq` 恒为库内原始值（phrase.freq 旧语义），
//!    加成只进排序（`user_overlay_test` 的 5001 断言同此契约）。
//! 3. 时间衰减——半衰期 30 天；拨旧 3 个半衰期后加成 900k→112.5k，排序回落。
//!    计数持久化在 kime_kv 旁表：重开库不丢提升，也不丢衰减基准日。
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::dict::{Candidate, Dict};

fn tmp_db(suffix: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_freq_boost_{}_{}_{}.sqlite",
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

fn seed_yaml(suffix: &str) -> PathBuf {
    let yaml = std::env::temp_dir().join(format!(
        "kime_freq_boost_seed_{}_{}.yaml",
        std::process::id(),
        suffix
    ));
    fs::write(&yaml, "...\n我们\two men\t500000\n我温\two men\t10\n").unwrap();
    yaml
}

fn texts(hits: &[Candidate]) -> Vec<&str> {
    hits.iter().map(|c| c.text.as_str()).collect()
}

fn wo_men_reading() -> Vec<String> {
    vec!["wo".to_string(), "men".to_string()]
}

#[test]
fn boost_calibrates_and_decays_without_touching_exported_freq() {
    let db = tmp_db("boost");
    let yaml = seed_yaml("boost");
    let mut d = Dict::open(&db).unwrap();
    d.import(&yaml).unwrap();
    let reading = wo_men_reading();

    // 基线：我温(10) 在 我们(500000) 之后。
    assert_eq!(texts(&d.lookup(&reading, 10).unwrap()), ["我们", "我温"]);

    // 第 1 次使用：n=1 → +300k（首用即满额，用户拍板 2026-09-15），
    // 压过 10⁵ 级长尾词、压不过 50 万级语料词。
    d.learn(&reading, "我温").unwrap();
    assert_eq!(
        texts(&d.lookup(&reading, 10).unwrap()),
        ["我们", "我温"],
        "300k 首用加成不得压过 freq=500,000 的语料词（BOOST 上界钉）"
    );

    // 第 2 次：600k > 500k，立即置顶（同一会话，无需重开库）。
    d.learn(&reading, "我温").unwrap();
    assert_eq!(
        texts(&d.lookup(&reading, 10).unwrap()),
        ["我温", "我们"],
        "n=2 必须压过 freq=500,000 的普通语料词（BOOST 校准钉）"
    );

    // 第 3 次：900k，置顶保持。
    d.learn(&reading, "我温").unwrap();
    assert_eq!(
        texts(&d.lookup(&reading, 10).unwrap()),
        ["我温", "我们"],
        "n=3 置顶保持"
    );

    // 导出频率仍是原始库值：加成绝不写进 Candidate.freq。
    let hits = d.lookup(&reading, 10).unwrap();
    let meiw = hits.iter().find(|c| c.text == "我温").unwrap();
    assert_eq!(meiw.freq, 13, "phrase.freq 旧语义：10 + 3 次 bump");

    // 持久化：重开库（模拟新会话），计数与排序都还在。
    drop(d);
    let d = Dict::open(&db).unwrap();
    assert_eq!(
        texts(&d.lookup(&reading, 10).unwrap()),
        ["我温", "我们"],
        "使用计数在 kime_kv 里持久化，重开库不得丢"
    );

    // 衰减：把最近使用日拨回 90 天（3 个半衰期），加成 900k→112.5k < 500k。
    let today = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        / 86_400;
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE kime_kv SET value = ?2 WHERE key = ?1",
        rusqlite::params!["wo'men\t我温", format!("3,{}", today - 90)],
    )
    .unwrap();
    drop(conn);
    let d = Dict::open(&db).unwrap();
    assert_eq!(
        texts(&d.lookup(&reading, 10).unwrap()),
        ["我们", "我温"],
        "3 个半衰期不用的用户词必须回落到语料词之后"
    );
    // 回落后导出频率依旧不变（衰减也不改 freq）。
    let hits = d.lookup(&reading, 10).unwrap();
    assert_eq!(hits.iter().find(|c| c.text == "我温").unwrap().freq, 13);

    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

/// 前缀查询的层一也吃提频（层一按 index 块序吐，learn 的块重排必须跟上）。
#[test]
fn boost_applies_to_prefix_layer_one() {
    let db = tmp_db("prefix");
    let yaml = seed_yaml("prefix");
    let mut d = Dict::open(&db).unwrap();
    d.import(&yaml).unwrap();
    let reading = wo_men_reading();
    for _ in 0..3 {
        d.learn(&reading, "我温").unwrap();
    }
    let hits = d.lookup_prefix(&["wo".to_string()], "men", 10).unwrap();
    assert_eq!(
        texts(&hits),
        ["我温", "我们"],
        "补全层一内 n=3 的用户词应置顶（当场重排 joined 块）"
    );
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

/// 缩写查询与精确/前缀查询同语义：n≥3 的用户词在 abbrev 命中里也置顶。
/// 两条路径（FST 走 merge_overlay、SQLite 走 abbrev_index 切片）必须都吃提频。
#[test]
fn boost_applies_to_abbrev_lookup() {
    let db = tmp_db("abbrev");
    let yaml = seed_yaml("abbrev");
    let mut d = Dict::open(&db).unwrap();
    d.import(&yaml).unwrap();
    // 「我们」与「我温」的缩写都是 wm；基线按裸频 我们(500000) 在前。
    assert_eq!(texts(&d.lookup_abbrev("wm", 10).unwrap()), ["我们", "我温"]);
    let reading = wo_men_reading();
    for _ in 0..3 {
        d.learn(&reading, "我温").unwrap();
    }
    assert_eq!(
        texts(&d.lookup_abbrev("wm", 10).unwrap()),
        ["我温", "我们"],
        "缩写查询必须同样按有效频率排序，否则打缩写时提频不生效"
    );
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}
