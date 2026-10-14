//! tail_ctx 新鲜度：引擎自持上下文尾巴，每次成功上屏把本次词追加进
//! `self.context` 并推进尾巴。否则一场输入会话内 `commit_log.tail_ctx` 冻结在
//! 会话语前文本，离线挖掘从中切出错误相邻对；热路径 `context_seed` 组句种子
//! 同样拿到陈旧上下文。
//!
//! 钉五件事（red 条件见对应断言）：
//! 1. 连续两次上屏：第二条 `commit_log.tail_ctx` 含第一次上屏文本（直接查库）；
//! 2. 会话前有上下文（`set_context(Some)`）时追加而非覆盖；
//! 3. 追加后超 32 字符上限按**字符边界**截断，不产 UTF-8 碎片；
//! 4. `set_context(None)` 后首次上屏：引擎内存尾巴 = 本次提交文本；
//! 5. Esc / 退格清组合不改变上下文（回归保护，与 context_test.rs 同族）。
//!
//! 与 tail_ctx_test.rs 分工：那边钉离线挖掘（Dict::log_commit 直接喂 tail），
//! 这边钉引擎上屏路径自推进尾巴（真实 `Engine::key` → 选词 commit_log 落库）。

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use kime_core::config::Config;
use kime_core::context::{append_tail, CONTEXT_TAIL_MAX_CHARS};
use kime_core::dict::Dict;
use kime_core::{Engine, Key, Outcome};

/// 临时库路径（pid + 纳秒唯一，防并行测试互踩）。
fn tmp_db(suffix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_tailfresh_{}_{}_{}.sqlite3",
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

/// 造两条同库词条（全拼）：「进度」jin'du、「世界」shi'jie，各为该拼音唯一候选。
fn pinyin_engine(db: &std::path::Path) -> Engine {
    let _ = Dict::open(db).expect("create schema");
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(
        "INSERT OR REPLACE INTO phrase(pinyin, text, freq, abbrev, user) VALUES
           ('jin''du','进度',9999,'jd',0),
           ('shi''jie','世界',9999,'sj',0);",
    )
    .unwrap();
    drop(conn);
    let dict = Dict::open(db).expect("reopen dict");
    let config = Config {
        dict_path: db.to_string_lossy().to_string(),
        shuangpin: None,
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

/// 逐字母进组合（全拼模式必须 Consumed）。
fn type_pinyin(e: &mut Engine, s: &str) {
    for c in s.chars() {
        assert_eq!(e.key(k(c)), Outcome::Consumed, "字母 {c:?} 必须进组合");
    }
}

/// 按「1」选当前页首候选上屏，断言上屏文本。
fn assert_commit(e: &mut Engine, expected: &str) {
    match e.key(k('1')) {
        Outcome::Commit(t) => assert_eq!(t, expected, "首候选应为 {expected}"),
        other => panic!("期望 Commit({expected})，实际 {other:?}"),
    }
}

/// 按 rowid 序读全部 `commit_log.tail_ctx`。
fn tail_ctx_rows(d: &Dict) -> Vec<Option<String>> {
    d.conn()
        .prepare("SELECT tail_ctx FROM commit_log ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get::<_, Option<String>>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

// ── 需求 1 + 2：连续两次上屏，第二条 tail_ctx 含第一次上屏文本（追加非覆盖） ──

#[test]
fn second_commit_tail_contains_first_committed_text() {
    let db = tmp_db("advance");
    let mut e = pinyin_engine(&db);
    // 会话前文「加回来」：模拟 surrounding_text 会话开始时给到的尾巴。
    e.set_context(Some("加回来".to_string()));

    // 第一次上屏：type jindu → 选 进度。此刻 context = 加回来，
    // 故 commit_log 第 1 行 tail_ctx = 加回来（上屏前文，不含本次词）。
    type_pinyin(&mut e, "jindu");
    assert_commit(&mut e, "进度");
    // 上屏后尾巴推进：加回来 + 进度（追加，非覆盖为 进度）。
    assert_eq!(e.context_tail(), Some("加回来进度"));

    // 第二次上屏：type shijie → 选 世界。此刻 context = 加回来进度，
    // 故 commit_log 第 2 行 tail_ctx = 加回来进度（含第一次上屏的 进度）。
    type_pinyin(&mut e, "shijie");
    assert_commit(&mut e, "世界");
    assert_eq!(e.context_tail(), Some("加回来进度世界"));

    // drop 排空后台写线程，重开库读 commit_log。
    drop(e);
    let d = Dict::open(&db).expect("reopen after drop");
    let rows = tail_ctx_rows(&d);
    assert_eq!(rows.len(), 2, "两次上屏 = 两条 commit_log 行");
    assert_eq!(
        rows[0].as_deref(),
        Some("加回来"),
        "第 1 行 tail = 上屏前文（未被第一次上屏覆盖/推进）"
    );
    assert_eq!(
        rows[1].as_deref(),
        Some("加回来进度"),
        "第 2 行 tail 含第一次上屏文本 进度（会话内尾巴前进）"
    );
    let _ = fs::remove_file(&db);
}

// ── 需求 4：set_context(None) 后首次上屏，内存尾巴 = 本次提交文本 ──────────

#[test]
fn first_commit_with_no_context_tail_is_committed_text() {
    let db = tmp_db("none");
    let mut e = pinyin_engine(&db);
    e.set_context(None);
    assert_eq!(e.context_tail(), None, "新建引擎无上下文");

    type_pinyin(&mut e, "jindu");
    assert_commit(&mut e, "进度");
    // 无会话前文：本次上屏后引擎内存尾巴 = 本次词 进度（不是空）。
    assert_eq!(
        e.context_tail(),
        Some("进度"),
        "set_context(None) 后首次上屏：尾巴 = 本次提交文本"
    );
    let _ = fs::remove_file(&db);
}

// ── 需求 3：超上限按字符边界截断，不产 UTF-8 碎片 ─────────────────────────

#[test]
fn append_tail_truncates_on_char_boundary() {
    // 30 个「上」+ 5 个「屏」= 35 字 > 32：只留末 32 字 = 27「上」+ 5「屏」。
    let current = "上".repeat(30);
    let committed = "屏".repeat(5);
    let out = append_tail(&current, &committed);
    let mut expected = "上".repeat(27);
    expected.push_str(&"屏".repeat(5));
    assert_eq!(
        out.chars().count(),
        CONTEXT_TAIL_MAX_CHARS,
        "截断到末 32 字符"
    );
    assert_eq!(out, expected, "保留末 32 字符");
    // CJK 每字 3 字节；32 字 = 96 字节，char 计数精确即无 UTF-8 碎片。
    assert_eq!(
        out.len(),
        CONTEXT_TAIL_MAX_CHARS * 3,
        "无碎片（字节数 = 32×3）"
    );

    // 未超上限：原样拼接返回，不截断。
    assert_eq!(append_tail("我们", "吃饭"), "我们吃饭");
    // 空 current（对应 context == None）：追加即本次词。
    assert_eq!(append_tail("", "进度"), "进度");
}

#[test]
fn commit_truncates_long_cjk_context_on_char_boundary() {
    // 40 字前文 + 2 字上屏 = 42 > 32：引擎内存尾巴 = 末 32 字 = 30「上」+「进度」。
    let db = tmp_db("trunc");
    let mut e = pinyin_engine(&db);
    e.set_context(Some("上".repeat(40)));
    type_pinyin(&mut e, "jindu");
    assert_commit(&mut e, "进度");

    let got = e.context_tail().expect("上屏后上下文必存在");
    assert_eq!(
        got.chars().count(),
        CONTEXT_TAIL_MAX_CHARS,
        "超长中文上下文按字符边界截断"
    );
    assert_eq!(
        got.to_string(),
        "上".repeat(30) + "进度",
        "末 32 字 = 前文末 30 + 本次词"
    );
    assert_eq!(got.len(), CONTEXT_TAIL_MAX_CHARS * 3, "无 UTF-8 碎片");
    let _ = fs::remove_file(&db);
}

// ── 需求 5：Esc / 退格清组合不改变上下文（回归保护） ──────────────────────

#[test]
fn clearing_composition_does_not_touch_context() {
    let db = tmp_db("esc");
    let mut e = pinyin_engine(&db);
    e.set_context(Some("我们".to_string()));

    // 打组合（ni 在本 fixture 无候选，字母照常累积）→ Esc 清组合。
    type_pinyin(&mut e, "ni");
    assert!(!e.preedit().is_empty(), "已有组合");
    let esc = Key {
        ch: None,
        code: 1, // KEY_ESC
        shift: false,
        ctrl: false,
        alt: false,
    };
    assert_eq!(e.key(esc), Outcome::Consumed);
    assert!(e.preedit().is_empty(), "组合已清空");
    assert_eq!(e.context_tail(), Some("我们"), "Esc 清组合不得改上下文");

    // 退格：空组合放行（Ignored），同样不得改上下文。
    let bs = Key {
        ch: None,
        code: 14, // KEY_BACKSPACE
        shift: false,
        ctrl: false,
        alt: false,
    };
    assert_eq!(e.key(bs), Outcome::Ignored, "空组合退格放行");
    assert_eq!(e.context_tail(), Some("我们"), "退格不得改上下文");
    let _ = fs::remove_file(&db);
}
