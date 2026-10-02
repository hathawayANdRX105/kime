//! 选词键路径的 SQLite 写移后台写线程（#101）：
//!
//! 1. 落库最终发生：引擎选词上屏后 `drop(engine)`（置停止标志 → 关 sender
//!    → join → 队列尾部排空），重开库读到 phrase 提频、commit_log、kime_kv
//!    计数——**忘了 enqueue / 共享 SQL 拆错 / drop 不 join 排空**，任一让它红。
//! 2. 内存提频不后台化：选词**当场**（不 drop、不等线程）再查同一拼音，
//!    刚学的词必须排到首位——**提频被误移进后台线程（或 learn_mem 拆丢
//!    user_stats/重排）**让它红。

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::{Engine, Key, Outcome};

fn tmp_db(suffix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_async_learn_{}_{}_{}.sqlite3",
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

/// 两条同拼音词条：「你好」freq=9999（默认首候选）、「你号」freq=100。
fn pinyin_engine(db: &std::path::Path) -> Engine {
    let _ = Dict::open(db).expect("create schema");
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(
        "INSERT OR REPLACE INTO phrase(pinyin, text, freq, abbrev, user) VALUES
           ('ni''hao','你好',9999,'nh',0),
           ('ni''hao','你号',100,'nh',0);",
    )
    .unwrap();
    drop(conn);
    let dict = Dict::open(db).expect("reopen dict");
    let config = Config {
        dict_path: db.to_string_lossy().to_string(),
        ..Config::default()
    };
    Engine::new(dict, config)
}

fn k(ch: char) -> Key {
    Key {
        ch: Some(ch),
        code: 0,
        shift: false,
        ctrl: false,
        alt: false,
    }
}

#[test]
fn engine_commit_persists_after_drop() {
    let db = tmp_db("drop");
    {
        let mut e = pinyin_engine(&db);
        assert!(e.candidates().is_empty(), "未输入时无候选");
        for c in "nih".chars() {
            e.key(k(c));
        }
        match e.key(k('1')) {
            Outcome::Commit(t) => assert_eq!(t, "你好"),
            other => panic!("expected Commit(你好), got {other:?}"),
        }
        // drop = 置停止标志 → 关 sender → join（队列尾部落盘后线程退出）。
        drop(e);
    }
    let d = Dict::open(&db).expect("reopen after drop");
    let (freq, user): (i64, i64) = d
        .conn()
        .query_row(
            "SELECT freq, user FROM phrase WHERE pinyin = 'ni''hao' AND text = '你好'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (freq, user),
        (10000, 1),
        "phrase UPSERT：9999+1 且标为用户词"
    );
    let commits: i64 = d
        .conn()
        .query_row("SELECT count(*) FROM commit_log", [], |r| r.get(0))
        .unwrap();
    assert_eq!(commits, 1, "一次选词上屏恰落一条 commit_log");
    let kv: String = d
        .conn()
        .query_row(
            "SELECT value FROM kime_kv WHERE key = 'ni''hao\t你好'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        kv.starts_with("1,"),
        "使用计数 n=1（值形如 1,天数），实际 {kv:?}"
    );
    let _ = fs::remove_file(&db);
}

#[test]
fn engine_learn_applies_in_memory_immediately() {
    let db = tmp_db("mem");
    let mut e = pinyin_engine(&db);
    assert!(e.candidates().is_empty(), "未输入时无候选");
    for c in "nih".chars() {
        e.key(k(c));
    }
    assert_eq!(
        e.candidates().first().map(|c| c.text.as_str()),
        Some("你好"),
        "fixture 下 freq=9999 的「你好」应为首候选"
    );
    // 选第二候选「你号」：触发 learn（内存提频当帧生效 + 持久化入队），不 drop。
    match e.key(k('2')) {
        Outcome::Commit(t) => assert_eq!(t, "你号"),
        other => panic!("expected Commit(你号), got {other:?}"),
    }
    for c in "nih".chars() {
        e.key(k(c));
    }
    assert_eq!(
        e.candidates().first().map(|c| c.text.as_str()),
        Some("你号"),
        "刚学的词当场排到首位——提频被移进后台线程即红"
    );
    drop(e);
    let _ = fs::remove_file(&db);
}
