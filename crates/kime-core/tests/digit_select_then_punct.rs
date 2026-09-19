//! 数字选词后的标点：数字被消费成中文候选上屏，不是输出字面数字，
//! after_digit 必须清零；否则紧随的 ,.: 被 digit_sep 分支当数字分隔符
//! 放行半角，用户在中文里看到英文逗号（「中文打字偶尔出现英文逗号」）。

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::{Engine, Key, Outcome};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn tmp_db() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_dsp_{}_{}.sqlite",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_file(&path);
    path
}

fn pinyin_engine(db: &std::path::Path) -> Engine {
    let _ = Dict::open(db).expect("create schema");
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(
        "INSERT OR REPLACE INTO phrase(pinyin, text, freq, abbrev, user) VALUES
           ('ni''hao','你好',9999,'nh',0);
        ",
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
fn digit_select_then_comma_is_full_width() {
    let db = tmp_db();
    let mut e = pinyin_engine(&db);
    for c in "nih".chars() {
        e.key(k(c));
    }
    assert_eq!(
        e.candidates().first().map(|c| c.text.clone()),
        Some("你好".to_string()),
        "fixture 下「你好」应为首候选"
    );
    // 1 选首候选「你好」
    match e.key(k('1')) {
        Outcome::Commit(t) => assert_eq!(t, "你好"),
        other => panic!("expected Commit(你好), got {:?}", other),
    }
    // 紧接着按逗号：必须是中文全角「，」
    match e.key(k(',')) {
        Outcome::Commit(t) => assert_eq!(t, "，", "digit 选词后逗号应为全角"),
        other => panic!("expected Commit(，), got {:?}", other),
    }
}

#[test]
fn literal_digit_then_comma_stays_half_width() {
    // 对照组：真的在输出数字（无候选、数字上屏）时，,.: 作为数字分隔符
    // 保持半角是正确行为，不能被上面的修复误伤。
    let db = tmp_db();
    let mut e = pinyin_engine(&db);
    // 对照组：无候选时空按 1，引擎不消费数字（Ignored 交给应用上屏），
    // 但 after_digit 已置位 → 紧随的 ,.: 走数字分隔符分支保持半角。
    // 断言不变量：绝不能因为修了选词场景就把这里的逗号也变成全角。
    match e.key(k('1')) {
        Outcome::Ignored => {}
        other => panic!("空 composition 下数字应放行给应用, got {:?}", other),
    }
    match e.key(k(',')) {
        Outcome::Commit(t) => assert_ne!(t, "，", "数字分隔符逗号不能变全角: {t:?}"),
        Outcome::Ignored => {}
        other => panic!("逗号应 Commit 半角或 Ignored, got {:?}", other),
    }
}
