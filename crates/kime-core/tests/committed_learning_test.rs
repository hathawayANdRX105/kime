//! 提交时学习：仅在 `Outcome::Commit` 的实际上屏文本上学习，候选入缓冲
//!（stage_candidate）不学习，取消（Esc）更不学。分词在已提交的精确文本上
//! 进行，标点/ASCII 断链，不跨段建立相邻对。

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::{Engine, Key, Outcome};

fn tmp_db(suffix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_committed_learning_{}_{}_{}.sqlite3",
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

/// 自然码：`ui` = shi。
fn engine(db: &Path) -> Engine {
    let _ = Dict::open(db).expect("create schema");
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(
        "INSERT OR REPLACE INTO phrase(pinyin, text, freq, abbrev, user) VALUES
           ('shi','是',9000,'s',0),
           ('shi','事',8000,'s',0);",
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

fn space() -> Key {
    Key {
        ch: None,
        code: 57,
        shift: false,
        ctrl: false,
        alt: false,
    }
}

fn esc() -> Key {
    Key {
        ch: None,
        code: 1,
        shift: false,
        ctrl: false,
        alt: false,
    }
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

fn commit_log_count(d: &Dict) -> i64 {
    d.conn()
        .query_row("SELECT count(*) FROM commit_log", [], |r| r.get(0))
        .unwrap()
}

fn phrase_user(d: &Dict, pinyin: &str, text: &str) -> i64 {
    d.conn()
        .query_row(
            "SELECT COALESCE((SELECT user FROM phrase WHERE pinyin = ?1 AND text = ?2), 0)",
            [pinyin, text],
            |r| r.get(0),
        )
        .unwrap()
}

fn type_keys(e: &mut Engine, input: &str) {
    for ch in input.chars() {
        e.key(k(ch));
    }
}

#[test]
fn canceled_preselection_learns_nothing() {
    let db = tmp_db("cancel");
    let mut e = engine(&db);
    type_keys(&mut e, "uiui");
    let syllable = e
        .candidates()
        .iter()
        .position(|candidate| candidate.pinyin == "shi")
        .expect("uiui 应含有只覆盖一个 shi 音节的字词");
    assert!(syllable < 9, "测试候选必须可用数字键选择");
    assert_eq!(
        e.key(k(char::from_digit((syllable + 1) as u32, 10).unwrap())),
        Outcome::Consumed
    );
    assert!(!e.letters().is_empty(), "预选单音节后仍应保留第二个音节");
    assert_eq!(e.key(esc()), Outcome::Consumed, "Esc 取消组合");
    drop(e);
    let d = Dict::open(&db).expect("reopen dict");
    assert_eq!(commit_log_count(&d), 0, "取消的预选不得落 commit_log");
    assert_eq!(phrase_user(&d, "shi", "是"), 0, "取消的预选不得提频");
    let kv: Option<String> = d
        .conn()
        .query_row(
            "SELECT value FROM kime_kv WHERE key = 'shi\t是'",
            [],
            |r| r.get(0),
        )
        .ok();
    assert!(kv.is_none(), "取消的预选不得写 kime_kv");
    let _ = fs::remove_file(&db);
}

#[test]
fn commit_learns_segmented_words_and_keeps_last_word_context() {
    let db = tmp_db("commit");
    let mut e = engine(&db);
    // 没有「是是」整词时，Viterbi 输出句子；提交时按词库分词为两个「是」。
    type_keys(&mut e, "uiui");
    let sentence = e
        .candidates()
        .iter()
        .position(|candidate| candidate.text == "是是")
        .expect("uiui 应能由词图组成「是是」");
    assert!(sentence < 9, "测试句候选必须可用数字键选择");
    assert_eq!(
        e.key(k(char::from_digit((sentence + 1) as u32, 10).unwrap())),
        Outcome::Commit("是是".into())
    );
    assert_eq!(e.span_cache_len(), 0, "提交学习后 span cache 应被清空");

    // 第二次独立 Commit 必须把首句末词作为相邻上文，不因一次性清空而丢失。
    type_keys(&mut e, "ui");
    assert_eq!(e.key(space()), Outcome::Commit("是".into()));
    drop(e);
    let d = Dict::open(&db).expect("reopen dict");
    let mut stmt = d
        .conn()
        .prepare(
            "SELECT v.text, c.reading, ctx.text, ctx.reading
             FROM commit_log c
             JOIN vocab v ON v.id = c.text_id
             LEFT JOIN vocab ctx ON ctx.id = c.ctx_id
             ORDER BY c.rowid",
        )
        .unwrap();
    let rows: Vec<(String, String, Option<String>, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(rows.len(), 3, "两个句内词和后续提交各落一条日志");
    assert_eq!(rows[0], ("是".into(), "shi".into(), None, None));
    assert_eq!(
        rows[1],
        (
            "是".into(),
            "shi".into(),
            Some("是".into()),
            Some("shi".into())
        )
    );
    assert_eq!(
        rows[2],
        (
            "是".into(),
            "shi".into(),
            Some("是".into()),
            Some("shi".into())
        )
    );
    assert_eq!(phrase_user(&d, "shi", "是"), 1, "phrase.user 是用户词标记");
    let uses: String = d
        .conn()
        .query_row(
            "SELECT value FROM kime_kv WHERE key = 'shi\t是'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        uses.starts_with("3,"),
        "三次提交的使用计数应为 3，实际 {uses:?}"
    );
    let _ = fs::remove_file(&db);
}

#[test]
fn punctuation_breaks_committed_word_adjacency() {
    let db = tmp_db("punct_break");
    let mut e = engine(&db);
    type_keys(&mut e, "ui");
    assert_eq!(e.key(space()), Outcome::Commit("是".into()));
    assert_eq!(e.key(k(',')), Outcome::Commit("，".into()));
    type_keys(&mut e, "ui");
    assert_eq!(e.key(space()), Outcome::Commit("是".into()));
    drop(e);
    let d = Dict::open(&db).expect("reopen dict");
    let mut stmt = d
        .conn()
        .prepare(
            "SELECT ctx_id IS NULL
             FROM commit_log c JOIN vocab v ON v.id = c.text_id
             WHERE v.text = '是' ORDER BY c.rowid",
        )
        .unwrap();
    let rows: Vec<bool> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(rows, [true, true], "标点必须断开前一提交词的相邻关系");
    let _ = fs::remove_file(&db);
}

#[test]
fn external_surrounding_update_breaks_commit_adjacency() {
    let db = tmp_db("external_edit");
    let mut e = engine(&db);
    type_keys(&mut e, "ui");
    assert_eq!(e.key(space()), Outcome::Commit("是".into()));
    e.set_context(Some("外部编辑".into()));
    for c in "ui".chars() {
        e.key(k(c));
    }
    assert_eq!(e.key(space()), Outcome::Commit("是".into()));
    drop(e);

    let d = Dict::open(&db).expect("reopen dict");
    let mut stmt = d
        .conn()
        .prepare(
            "SELECT c.ctx_id IS NULL, c.tail_ctx
             FROM commit_log c JOIN vocab v ON v.id = c.text_id
             WHERE v.text = '是' ORDER BY c.rowid",
        )
        .unwrap();
    let rows: Vec<(bool, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(rows[0], (true, None));
    assert_eq!(rows[1], (true, Some("外部编辑".into())));
    let _ = fs::remove_file(&db);
}
