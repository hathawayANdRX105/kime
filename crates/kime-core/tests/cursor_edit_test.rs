//! 组合内光标编辑（用户要求）：C-b 左移、C-f 右移、C-h 删光标前一字符；
//! 字母在光标处插入；翻页让位（Ctrl+F/B 不再翻页，- / = 与 Ctrl+N/P 保留）。
//! 空组合下 Ctrl+F/B/C/H 一律 Ignored —— 应用的 emacs 移动键与复制必须原样可达。

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::{Engine, Key, Outcome};

const KEY_BACKSPACE: u32 = 14;

fn tmp_db(prefix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_cursor_{}_{}_{}.sqlite",
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
    ('ni''hao','拟好',4000,'nh',0),
    ('a','安',8000,'a',0);";

#[test]
fn insertion_happens_at_cursor_not_at_tail() {
    let db = tmp_db("insert");
    let mut e = engine_with(&db, FIX_ROWS, 10);
    type_str(&mut e, "nihao");
    assert_eq!(e.preedit(), "nihao");
    assert_eq!(e.cursor(), 5, "打字时光标贴尾");
    assert_eq!(e.preedit_cursor(), 5, "报给应用的光标默认在尾");

    assert_eq!(e.key(ctrl_ch('b')), Outcome::Consumed);
    assert_eq!(e.key(ctrl_ch('b')), Outcome::Consumed);
    assert_eq!(e.cursor(), 3, "C-b×2 → 光标在 3（'nih|ao'）");
    assert_eq!(e.preedit_cursor(), 3, "应用侧 caret 同步");
    assert_eq!(e.preedit(), "nihao", "移动不改组合内容");

    assert_eq!(e.key(ch('x')), Outcome::Consumed);
    // 尾部追加会得到 "nihaox"；光标处插入得到 "nih"+"x"+"ao"。
    assert_eq!(e.preedit(), "nihxao", "插入必须发生在光标处而不是尾部");
    assert_eq!(e.cursor(), 4);

    assert_eq!(e.key(ctrl_ch('f')), Outcome::Consumed);
    assert_eq!(e.cursor(), 5, "C-f 回到尾");
    assert_eq!(e.preedit_cursor(), 5, "应用侧 caret 回尾");
    let _ = fs::remove_file(&db);
}

#[test]
fn out_of_bounds_moves_are_noops_without_panic() {
    let db = tmp_db("oob");
    let mut e = engine_with(&db, FIX_ROWS, 10);
    type_str(&mut e, "nihao");
    for _ in 0..5 {
        e.key(ctrl_ch('b'));
    }
    assert_eq!(e.cursor(), 0);
    // 越界：在 0 按 C-b —— 消费但状态纹丝不动（组合在手，不放幽灵键给应用）。
    assert_eq!(e.key(ctrl_ch('b')), Outcome::Consumed);
    assert_eq!(e.cursor(), 0);
    assert_eq!(e.preedit(), "nihao");
    // 越界：在尾按 C-f 同样钳位。
    for _ in 0..6 {
        e.key(ctrl_ch('f'));
    }
    assert_eq!(e.cursor(), 5);
    assert_eq!(e.preedit(), "nihao");
    let _ = fs::remove_file(&db);
}

#[test]
fn ctrl_h_and_backspace_delete_char_before_cursor() {
    let db = tmp_db("del");
    let mut e = engine_with(&db, FIX_ROWS, 10);
    type_str(&mut e, "nihao");
    e.key(ctrl_ch('b')); // niha|o, cursor 4
    assert_eq!(e.key(ctrl_ch('h')), Outcome::Consumed);
    assert_eq!(e.preedit(), "niho", "C-h 删的是光标前的 'a'");
    assert_eq!(e.cursor(), 3);

    // Backspace 与 C-h 同语义：删光标前一个字符（此处是 'h'），不是无条件 pop 尾。
    assert_eq!(e.key(code(KEY_BACKSPACE)), Outcome::Consumed);
    assert_eq!(e.preedit(), "nio");
    assert_eq!(e.cursor(), 2);

    // 光标在 0：C-h / Backspace 都是消费但无操作。
    e.key(ctrl_ch('b'));
    assert_eq!(e.cursor(), 1);
    e.key(ctrl_ch('b'));
    assert_eq!(e.cursor(), 0);
    assert_eq!(e.key(ctrl_ch('h')), Outcome::Consumed);
    assert_eq!(e.preedit(), "nio");
    assert_eq!(e.key(code(KEY_BACKSPACE)), Outcome::Consumed);
    assert_eq!(e.preedit(), "nio", "光标前的删除不动光标后的字符");

    // 删空组合：与 Backspace 一致 —— preedit/候选清空、光标归位。
    let mut f = engine_with(&tmp_db("del2"), FIX_ROWS, 10);
    type_str(&mut f, "a");
    assert_eq!(f.key(ctrl_ch('h')), Outcome::Consumed);
    assert!(f.preedit().is_empty(), "C-h 删空后组合必须清空");
    assert!(f.candidates().is_empty());
    assert_eq!(f.cursor(), 0);
    // 空组合后 Backspace 转交应用
    assert_eq!(f.key(code(KEY_BACKSPACE)), Outcome::Ignored);
    let _ = fs::remove_file(&db);
}

#[test]
fn ctrl_letters_unrelated_to_editing_are_ignored() {
    let db = tmp_db("ignored");
    let mut e = engine_with(&db, FIX_ROWS, 10);
    // 无组合：Ctrl+C 必须放行（应用要收到复制）。
    assert_eq!(e.key(ctrl_ch('c')), Outcome::Ignored);
    assert_eq!(e.key(ctrl_ch('f')), Outcome::Ignored, "空组合 C-f 归应用");
    assert_eq!(e.key(ctrl_ch('b')), Outcome::Ignored, "空组合 C-b 归应用");
    assert_eq!(e.key(ctrl_ch('h')), Outcome::Ignored, "空组合 C-h 归应用");
    // 有组合、有候选：Ctrl+C 依然放行 —— 只拦 b/f/h 三个。
    type_str(&mut e, "nihao");
    assert!(!e.candidates().is_empty(), "前提：nihao 有候选");
    assert_eq!(e.key(ctrl_ch('c')), Outcome::Ignored);
    assert_eq!(e.key(ctrl_ch('v')), Outcome::Ignored);
    assert_eq!(e.cursor(), 5, "被放行的组合键不碰引擎状态");
    let _ = fs::remove_file(&db);
}

#[test]
fn cursor_moves_do_not_disturb_candidate_layers() {
    let db = tmp_db("layers");
    let mut e = engine_with(&db, FIX_ROWS, 10);
    type_str(&mut e, "nihao");
    let before: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
    assert!(before.iter().any(|t| t == "你好"), "前提：fixture 命中");
    for _ in 0..4 {
        e.key(ctrl_ch('b'));
    }
    e.key(ctrl_ch('f'));
    let after: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
    assert_eq!(before, after, "光标移动不得重排/清空候选（分层契约）");
    assert_eq!(e.page().0, 0, "光标移动不碰页码");
    let _ = fs::remove_file(&db);
}

#[test]
fn ctrl_f_b_yielded_paging_to_ctrl_n_p_and_minus_equal() {
    let db = tmp_db("paging");
    let mut e = engine_with(&db, FIX_ROWS, 1); // 每页 1 条，"ni" 前缀 ≥2 条候选
    type_str(&mut e, "ni");
    assert!(e.candidates().len() >= 2, "前提：多于一页");
    assert_eq!(e.page().0, 0);
    // Ctrl+F：光标语义接管，翻页让位（用户要求的取舍）。
    assert_eq!(e.key(ctrl_ch('f')), Outcome::Consumed);
    assert_eq!(e.page().0, 0, "Ctrl+F 不再翻页");
    // Ctrl+B 同理。
    assert_eq!(e.key(ctrl_ch('b')), Outcome::Consumed);
    assert_eq!(e.page().0, 0, "Ctrl+B 不再翻页");
    // Ctrl+N / Ctrl+P 保留翻页。
    assert_eq!(e.key(ctrl_ch('n')), Outcome::Consumed);
    assert_eq!(e.page().0, 1, "Ctrl+N 下一页");
    assert_eq!(e.key(ctrl_ch('p')), Outcome::Consumed);
    assert_eq!(e.page().0, 0, "Ctrl+P 上一页");
    // - / = 字符路径翻页保留。
    assert_eq!(e.key(ch('=')), Outcome::Consumed);
    assert_eq!(e.page().0, 1, "= 下一页");
    assert_eq!(e.key(ch('-')), Outcome::Consumed);
    assert_eq!(e.page().0, 0, "- 上一页");
    let _ = fs::remove_file(&db);
}
