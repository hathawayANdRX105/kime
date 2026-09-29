//! 标点上屏 + Delete 前删（fcitx5 cancelLast / context_.del() 语义）。
//!
//! - 智能标点上屏（中文模式 "."→"。" 等实际转换）后，空闲态退格 = `Ignored`
//!   放行应用删全角字符（不还原半角原键）。
//! - `KEY_DELETE`（evdev 11）组合内前删：删光标后 1 个拼音字符；组合空放行。
//!
//! fixture 模式与同目录 cursor_edit_test.rs 一致（临时 SQLite 词库）。

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::{Engine, Key, Outcome};

const KEY_BACKSPACE: u32 = 14;
const KEY_DELETE: u32 = 11;

fn tmp_db(prefix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_puncundo_{}_{}_{}.sqlite",
        prefix,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_file(&path);
    path
}

fn engine_with(db: &std::path::Path, rows: &str, page_size: usize) -> Engine {
    let _ = Dict::open(db).expect("create schema");
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(rows).unwrap();
    drop(conn);
    let dict = Dict::open(db).expect("reopen dict");
    Engine::new(
        dict,
        Config {
            shuangpin: None,
            page_size,
            ..Config::default()
        },
    )
}

fn ch(c: char) -> Key {
    Key {
        ch: Some(c),
        code: 0,
        shift: false,
        ctrl: false,
        alt: false,
    }
}

fn ctrl_ch(c: char) -> Key {
    Key {
        ch: Some(c),
        code: 0,
        shift: false,
        ctrl: true,
        alt: false,
    }
}

fn code(code: u32) -> Key {
    Key {
        ch: None,
        code,
        shift: false,
        ctrl: false,
        alt: false,
    }
}

fn type_str(e: &mut Engine, s: &str) {
    for c in s.chars() {
        assert_eq!(e.key(ch(c)), Outcome::Consumed, "字母 {c} 应被消费");
    }
}

const FIX_ROWS: &str = "INSERT OR REPLACE INTO phrase(pinyin,text,freq,abbrev,user) VALUES
    ('ni''hao','你好',5000,'nh',0),
    ('ni''hou','泥猴',100,'nh',0),
    ('a','安',8000,'a',0);";

#[test]
fn punc_backspace_now_ignored() {
    // "." 上屏「。」后空闲退格 = Ignored 放行应用删全角字符。
    let db = tmp_db("bs");
    let mut e = engine_with(&db, FIX_ROWS, 10);
    assert_eq!(e.key(ch('.')), Outcome::Commit("。".into()));
    // 第二次退格同样 Ignored（应用可继续删）
    assert_eq!(e.key(code(KEY_BACKSPACE)), Outcome::Ignored);
    let _ = fs::remove_file(&db);
}

#[test]
fn punc_into_composition_still_consumed() {
    // "." 上屏后按 "n" 入组合 → 退格删的是拼音（Consumed），
    // 不再涉及撤销态。
    let db = tmp_db("letter");
    let mut e = engine_with(&db, FIX_ROWS, 10);
    assert_eq!(e.key(ch('.')), Outcome::Commit("。".into()));
    assert_eq!(e.key(ch('n')), Outcome::Consumed);
    assert_eq!(
        e.key(code(KEY_BACKSPACE)),
        Outcome::Consumed,
        "退格删拼音 'n' 为 Consumed"
    );
    let _ = fs::remove_file(&db);
}

#[test]
fn delete_removes_after_caret() {
    // 尾部无前删对象（"niha|"）：Delete 消费但不动串（防吞成应用前删）；
    // C-b 移到 "nih|a" 后 Delete 删 'a'。防「前删错位」bug：误删光标前
    // 字符则 C-b 一步后 Delete 变 "ni"（同退格/C-h 方向即回归）。
    let db = tmp_db("del");
    let mut e = engine_with(&db, FIX_ROWS, 10);
    type_str(&mut e, "niha");
    assert_eq!(e.preedit(), "niha");
    assert_eq!(e.key(code(KEY_DELETE)), Outcome::Consumed);
    assert_eq!(e.preedit(), "niha", "尾部 Delete 无前删对象");
    assert_eq!(e.key(ctrl_ch('b')), Outcome::Consumed);
    assert_eq!(e.cursor(), 3, "C-b 光标左移一位");
    assert_eq!(e.key(code(KEY_DELETE)), Outcome::Consumed);
    assert_eq!(e.preedit(), "nih", "Delete 删光标后字符");
    let _ = fs::remove_file(&db);
}

#[test]
fn delete_idle_forwarded() {
    // 组合空（未输入任何拼音）按 Delete → Ignored 放行应用前删
    // （防「吞应用删键」bug：空组合不得吞掉应用的 Delete）。
    let db = tmp_db("idle");
    let mut e = engine_with(&db, FIX_ROWS, 10);
    assert_eq!(e.key(code(KEY_DELETE)), Outcome::Ignored);
    let _ = fs::remove_file(&db);
}
