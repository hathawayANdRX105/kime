//! 智能标点撤销 + Delete 前删（#85，对齐 fcitx5 cancelLast / context_.del()）。
//!
//! - 智能标点上屏（中文模式 "."→"。" 等实际转换）后，空闲态退格发
//!   `Outcome::PuncCancel`（一次性：发出即清，第二次退格落 Ignored）。
//! - 任何其它键进组合即清除撤销态（字母入组合 / 再标点 / 空格 / Esc…）。
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

/// 断言 `key` 返回 PuncCancel 且 (original, fullwidth) 与期望一致。
fn assert_punc_cancel(outcome: Outcome, original: &str, fullwidth: &str) {
    match outcome {
        Outcome::PuncCancel {
            original: got_o,
            fullwidth: got_f,
        } => {
            assert_eq!(got_o, original, "原按键串");
            assert_eq!(got_f, fullwidth, "上屏全角串");
        }
        other => panic!("期望 PuncCancel({original}<-{fullwidth})，实际 {other:?}"),
    }
}

#[test]
fn punc_backspace_restores_original() {
    // "." 上屏「。」后空闲退格 → PuncCancel{".", "。"}；撤销态一次性——
    // 紧接着第二次退格必须 Ignored（防「退格连环撤销」bug）。
    let db = tmp_db("bs");
    let mut e = engine_with(&db, FIX_ROWS, 10);
    assert_eq!(e.key(ch('.')), Outcome::Commit("。".into()));
    assert_punc_cancel(e.key(code(KEY_BACKSPACE)), ".", "。");
    assert!(e.last_punc().is_none(), "PuncCancel 发出即清撤销态");
    assert_eq!(
        e.key(code(KEY_BACKSPACE)),
        Outcome::Ignored,
        "第二次退格放行应用"
    );
    let _ = fs::remove_file(&db);
}

#[test]
fn punc_state_cleared_by_letter() {
    // "." 上屏后按 "n" 入组合 → 撤销态必须清除；退格删的是拼音（Consumed），
    // 不是 PuncCancel（防「过期撤销」bug：字母已进组合再退格误删上屏标点）。
    let db = tmp_db("letter");
    let mut e = engine_with(&db, FIX_ROWS, 10);
    assert_eq!(e.key(ch('.')), Outcome::Commit("。".into()));
    assert_eq!(e.key(ch('n')), Outcome::Consumed);
    assert!(e.last_punc().is_none(), "字母入组合即清除标点撤销态");
    assert_eq!(
        e.key(code(KEY_BACKSPACE)),
        Outcome::Consumed,
        "退格删拼音 'n'，不得发 PuncCancel"
    );
    let _ = fs::remove_file(&db);
}

#[test]
fn punc_state_cleared_by_second_punc() {
    // "." 之后再按 "?"（又一次智能标点上屏）→ 退格还原的是「最新一次」
    // "?"（防「旧状态覆盖」bug：撤销态必须被新标点覆盖而不是残留）。
    let db = tmp_db("second");
    let mut e = engine_with(&db, FIX_ROWS, 10);
    assert_eq!(e.key(ch('.')), Outcome::Commit("。".into()));
    assert_eq!(e.key(ch('?')), Outcome::Commit("？".into()));
    assert_punc_cancel(e.key(code(KEY_BACKSPACE)), "?", "？");
    let _ = fs::remove_file(&db);
}

#[test]
fn delete_removes_after_caret() {
    // "niha" 按 Delete → 删光标后 1 个拼音字符 → "nia"（防「前删错位」bug：
    // 误删光标前字符会变 "nih"，与退格/C-h 同向）。
    let db = tmp_db("del");
    let mut e = engine_with(&db, FIX_ROWS, 10);
    type_str(&mut e, "niha");
    assert_eq!(e.preedit(), "niha");
    assert_eq!(e.key(code(KEY_DELETE)), Outcome::Consumed);
    assert_eq!(e.preedit(), "nia", "Delete 删光标后字符");
    // 组合内连续前删到光标前不动：再删 'i'。
    assert_eq!(e.key(code(KEY_DELETE)), Outcome::Consumed);
    assert_eq!(e.preedit(), "na");
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
