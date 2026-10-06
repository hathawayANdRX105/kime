//! builder 集成测试：原位于 src/builder.rs 的内嵌测试模块（编译期测试配置），
//! 迁移后经 kime_core 公共 API（`kime_core::builder::build` /
//! `kime_core::store::FstStore`）驱动；`seed_sqlite` 辅助函数保留为文件内私有 fn。

use kime_core::builder::build;
use kime_core::store::FstStore;
use rusqlite::params;
use rusqlite::Connection;
use std::path::PathBuf;
use tempfile::NamedTempFile;

fn seed_sqlite(path: &PathBuf) {
    let conn = Connection::open(path).unwrap();
    conn.execute(
        "CREATE TABLE phrase (pinyin TEXT NOT NULL, text TEXT NOT NULL, freq INTEGER NOT NULL DEFAULT 0, abbrev TEXT NOT NULL, user INTEGER NOT NULL DEFAULT 0, UNIQUE(pinyin,text))",
        [],
    ).unwrap();
    let mut stmt = conn
        .prepare("INSERT INTO phrase(pinyin,text,freq,abbrev,user) VALUES (?1,?2,?3,?4,0)")
        .unwrap();
    for (p, t, f) in [
        ("ni'hao", "你好", 5000i64),
        ("ni'hao", "拟好", 100),
        ("shen'me", "什么", 8000),
        ("shen'me", "审美", 500),
    ] {
        stmt.execute(params![p, t, f, "sm"]).unwrap();
    }
}

#[test]
fn build_then_open_roundtrip() {
    let db = NamedTempFile::new().unwrap();
    seed_sqlite(&db.path().to_path_buf());
    let bin = NamedTempFile::new().unwrap();
    let n = build(db.path(), bin.path()).unwrap();
    assert_eq!(n, 4, "应写入 4 条候选");

    let store = FstStore::open(bin.path()).unwrap();
    let hits = store.lookup_prefix(&["ni".into()], "hao", 10);
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].text, "你好"); // freq 5000 > 100
    assert_eq!(hits[1].text, "拟好");

    let sm = store.lookup_prefix(&["shen".into()], "me", 10);
    assert_eq!(sm.len(), 2);
    assert_eq!(sm[0].text, "什么");

    // abbrev
    let abbrev = store.lookup_abbrev("sm", 10);
    assert_eq!(abbrev.len(), 2);

    // 不存在
    let none = store.lookup_prefix(&["zzz".into()], "", 10);
    assert!(none.is_empty());
}
