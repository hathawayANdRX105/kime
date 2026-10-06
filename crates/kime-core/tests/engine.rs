//! engine 集成测试：原位于 src/engine.rs 的内嵌测试模块（编译期测试配置），
//! 迁移后经 kime_core 公共 API（`kime_core::engine` / `kime_core::config` /
//! `kime_core::dict`）驱动；引擎内部状态经 `letters()` / `last_reading()` /
//! `punct_mode()` 等诊断访问器读取。
//! 辅助函数（tmp_db / fixture_yaml / fixture_yaml_path / engine_with_fixture /
//! engine_with_shuangpin_fixture / fixture_many_ni / k / code_k / shift_k /
//! ctrl_h_k / engine_with_sp_sentence_fixture /
//! engine_with_full_pinyin_pending_fixture / FUZZY_YAML）保留为文件内私有 fn。

use kime_core::config::{Config, PunctMode};
use kime_core::dict::{Candidate, Dict};
use kime_core::engine::{Engine, Key, Outcome};
use kime_core::engine::{KEY_BACKSPACE, KEY_EQUAL, KEY_ESC, KEY_LEFTSHIFT, KEY_MINUS, KEY_SPACE};
use kime_shuangpin::Scheme;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn tmp_db(suffix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_engine_{}_{}_{}.sqlite",
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

fn fixture_yaml() -> &'static str {
    // 简洁但覆盖多音节 / 单字 / 半截尾命中路径：
    // - "你好" / "泥猴" 都以 "ni" 开头，便于测试半截尾 "h" → "ha"/"hou"。
    // - "世界" 测全拼完整匹配。
    // - "安" 单字 pinyin 测空 syllables + tail。
    "\
...\n你好\tni hao\t5000\n\
泥猴\tni hou\t100\n\
世界\tshi jie\t9999\n\
安\ta\t8000\n\
啊啊\ta a\t1\n\
"
}

fn fixture_yaml_path() -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!(
        "kime_engine_yaml_{}_{}.yaml",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::write(&p, fixture_yaml()).unwrap();
    p
}

fn engine_with_fixture() -> (Engine, std::path::PathBuf, std::path::PathBuf) {
    let db = tmp_db("main");
    let yaml = fixture_yaml_path();
    let _dict = Dict::open(&db).expect("open dict");
    // 直接 INSERT 行：fixture_yaml 可能因格式敏感失败，回退到 raw SQL。
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "INSERT OR REPLACE INTO phrase(pinyin, text, freq, abbrev, user) VALUES
           ('ni''hao','你好',5000,'nh',0),
           ('ni''hou','泥猴', 100,'nh',0),
           ('shi''jie','世界',9999,'sj',0),
           ('a',     '安',  8000,'a', 0),
           ('a''a',  '啊啊',   1,'a', 0);
        ",
    )
    .unwrap();
    drop(conn);
    // dict connection is stale now; reopen for lookups.
    let dict = Dict::open(&db).expect("reopen dict for lookups");
    let config = Config {
        shuangpin: None,
        ..Config::default()
    };
    let engine = Engine::new(dict, config);
    (engine, db, yaml)
}

fn engine_with_shuangpin_fixture() -> (Engine, std::path::PathBuf, std::path::PathBuf) {
    let db = tmp_db("shuangpin");
    let yaml = fixture_yaml_path();
    let _dict = Dict::open(&db).expect("open dict");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "INSERT OR REPLACE INTO phrase(pinyin, text, freq, abbrev, user) VALUES
           ('ni''hao','你好',5000,'nh',0),
           ('ni''hou','泥猴', 100,'nh',0),
           ('shi''jie','世界',9999,'sj',0),
           ('a',     '安',  8000,'a', 0),
           ('a''a',  '啊啊',   1,'a', 0),
           ('ni''hao','你好',5000,'h%',0),
           ('ni''hao','你好',5000,'x',0),
           ('fan''gan','反感',600,'fg',0),
           ('fang''an','方案',900,'fa',0);
        ",
    )
    .unwrap();
    drop(conn);
    let dict = Dict::open(&db).expect("reopen dict for lookups");
    let config = Config {
        dict_path: db.to_string_lossy().to_string(),
        shuangpin: Some(Scheme::Xiaohe),
        ..Config::default()
    };
    let engine = Engine::new(dict, config);
    (engine, db, yaml)
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

fn code_k(code: u32) -> Key {
    Key {
        ch: None,
        code,
        shift: false,
        ctrl: false,
        alt: false,
    }
}

fn shift_k(code: u32) -> Key {
    Key {
        ch: None,
        code,
        shift: true,
        ctrl: false,
        alt: false,
    }
}

/// C-h 光标编辑键（Ctrl+H，evdev 码 35）。
fn ctrl_h_k() -> Key {
    Key {
        ch: Some('h'),
        code: 35,
        shift: false,
        ctrl: true,
        alt: false,
    }
}

#[test]
fn shuangpin_even_length_input() {
    let (mut e, db, yaml) = engine_with_shuangpin_fixture();
    // 小鹤码 nihc -> ni + hao
    for c in "nihc".chars() {
        e.key(k(c));
    }
    // preedit 应为拼音 "nihao"
    assert_eq!(e.preedit(), "nihao");
    // last_reading 应为 ["ni", "hao"]
    assert_eq!(e.last_reading(), vec!["ni".to_string(), "hao".to_string()]);
    // 候选应含 "你好"
    let cs: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
    assert!(cs.contains(&"你好".to_string()));
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn shuangpin_odd_length_input() {
    let (mut e, db, yaml) = engine_with_shuangpin_fixture();
    // nih -> ni + "h" 尾部前缀
    for c in "nih".chars() {
        e.key(k(c));
    }
    // preedit 应为拼音 "ni" + tail "h"
    assert_eq!(e.preedit(), "nih");
    // last_reading 应为 ["ni"]
    assert_eq!(e.last_reading(), vec!["ni".to_string()]);
    // tail "h" 应触发 "ha" 前缀匹配，候选应含 "你好"
    let cs: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
    assert!(cs.contains(&"你好".to_string()));
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn digit_selection_triggers_learn_and_reorders_candidates() {
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "nih".chars() {
        e.key(k(c));
    }
    // 1 选词 "你好"（覆盖全部输入 = 立即上屏），应触发 learn -> 候选顺序变化
    let outcome = e.key(k('1'));
    match outcome {
        Outcome::Commit(t) => assert_eq!(t, "你好"),
        other => panic!("expected Commit, got {:?}", other),
    }
    // 再查询，确保排序已更新（用户词频率更高）
    let mut e2 = Engine::new(Dict::open(&db).unwrap(), Config::default());
    for c in "nih".chars() {
        e2.key(k(c));
    }
    // 现在 "你好" 频率高，排在第一
    assert_eq!(e2.candidates().first().unwrap().text, "你好");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn space_selection_triggers_learn_and_clears() {
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "nih".chars() {
        e.key(k(c));
    }
    // 空格选词（覆盖全部输入）= 立即上屏，显示串清空
    match e.key(code_k(KEY_SPACE)) {
        Outcome::Commit(t) => assert_eq!(t, "你好"),
        other => panic!("expected Commit, got {:?}", other),
    }
    assert!(e.preedit().is_empty(), "上屏后显示串清空");
    assert!(e.candidates().is_empty());
    // 学习后再次查询，用户词频率更高
    let mut e2 = Engine::new(Dict::open(&db).unwrap(), Config::default());
    for c in "nih".chars() {
        e2.key(k(c));
    }
    assert_eq!(e2.candidates().first().unwrap().text, "你好");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn full_pinyin_abbrev_fallback() {
    let (mut e, db, yaml) = engine_with_fixture();
    // 打 "nh" -> abbrev 匹配 "nh" 对应 "你好"/"泥猴"
    for c in "nh".chars() {
        e.key(k(c));
    }
    // abbrev 兜底时 last_reading 应清空
    assert!(e.last_reading().is_empty());
    // 候选应含 "你好"
    let cs: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
    assert!(cs.contains(&"你好".to_string()));
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

/// 双拼模式下轻点 Shift 切英文：字母/数字全 Ignored 直通，组合与候选清空。
#[test]
fn english_mode_disables_shuangpin() {
    let (mut e, db, yaml) = engine_with_shuangpin_fixture();
    assert_eq!(e.key(shift_k(KEY_LEFTSHIFT)), Outcome::Consumed);
    // 英文模式下所有按键 Ignored，组合/候选为空
    assert!(!e.chinese());
    assert!(e.preedit().is_empty());
    assert!(e.candidates().is_empty());
    assert_eq!(e.key(k('a')), Outcome::Ignored, "英文模式字母直通");
    assert_eq!(e.key(k('1')), Outcome::Ignored, "英文模式数字直通");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn shuangpin_preedit_shows_decoded_pinyin() {
    let (mut e, db, yaml) = engine_with_shuangpin_fixture();
    // 打 "nihc" -> 小鹤解码 nihao
    for c in "nihc".chars() {
        e.key(k(c));
    }
    // preedit 应为解码出的拼音 "nihao"，而不是原字母 "nihc"
    assert_eq!(e.preedit(), "nihao");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn accumulates_letters_into_preedit_and_candidates() {
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "nih".chars() {
        assert_eq!(e.key(k(c)), Outcome::Consumed);
    }
    assert_eq!(e.preedit(), "nih");
    let cs: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
    assert!(
        cs.contains(&"你好".to_string()),
        "half-tail 'h' should match ni'hao, got {:?}",
        cs
    );
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn half_syllable_shows_prefix_candidates() {
    // "niha" → 切 ["ni","ha"]；按规则 tail = "ha"、reading = ["ni"]，
    // 所以 LIKE 'ni''ha%' 应命中 "ni'hao" → "你好"。
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "niha".chars() {
        e.key(k(c));
    }
    assert_eq!(e.preedit(), "niha");
    let cs: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
    assert!(
        cs.contains(&"你好".to_string()),
        "niha should still surface 你好 via ni'ha% prefix, got {:?}",
        cs
    );
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn digit_selects_candidate_and_commits() {
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "nih".chars() {
        e.key(k(c));
    }
    let outcome = e.key(k('1'));
    match outcome {
        Outcome::Commit(t) => assert_eq!(t, "你好"),
        other => panic!("expected Commit, got {:?}", other),
    }
    assert!(e.preedit().is_empty(), "候选覆盖全部输入 = 立即上屏");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn space_commits_top_candidate() {
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "nih".chars() {
        e.key(k(c));
    }
    match e.key(code_k(KEY_SPACE)) {
        Outcome::Commit(t) => assert_eq!(t, "你好"),
        other => panic!("expected Commit, got {:?}", other),
    }
    assert!(e.preedit().is_empty());
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn digit_without_candidates_is_ignored() {
    let (mut e, db, yaml) = engine_with_fixture();
    assert_eq!(e.key(k('1')), Outcome::Ignored);
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn space_without_candidates_is_ignored() {
    let (mut e, db, yaml) = engine_with_fixture();
    assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Ignored);
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn backspace_removes_last_letter() {
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "nih".chars() {
        e.key(k(c));
    }
    assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Consumed);
    assert_eq!(e.preedit(), "ni");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn backspace_on_empty_is_ignored() {
    let (mut e, db, yaml) = engine_with_fixture();
    assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Ignored);
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

/// 部分选词（选词只消耗已选音节）后光标必须落在剩余拼音末尾：
/// 后续打字续在尾部而不是插进剩余拼音头部（2.1「光标不是最后」回归）。
#[test]
fn partial_selection_keeps_caret_at_tail() {
    let (mut e, db, yaml) = engine_with_fixture();
    // nihaoni：[ni,hao,ni] 分段，唯一候选 你好（fixture 无单字「你」），
    // 选词消耗 ni+hao，剩 "ni"
    for c in "nihaoni".chars() {
        e.key(k(c));
    }
    // #81：空格选词入 pending（不上屏），显示串 = pending + 剩余拼音
    assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
    assert_eq!(e.letters(), "ni", "剩余拼音必须保留");
    assert_eq!(e.preedit(), "你好ni", "显示串含预选词");
    assert_eq!(e.cursor(), e.letters().len(), "光标在剩余拼音末尾");
    assert_eq!(e.preedit_cursor(), 4, "caret = 你好(2) + ni(2)");
    e.key(k('h'));
    assert_eq!(e.letters(), "nih", "新字符续在剩余拼音尾部");
    assert_eq!(e.preedit(), "你好nih");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

/// #80 上屏 = 撤销链死亡：全量选词 Commit 出口经 `release_words`
/// 清 `undo_consumed`（#79 跨上屏 LIFO 语义作废）；此后空闲退格
/// = Ignored 放行应用删字，不再弹词还原拼音。
#[test]
fn full_selection_commit_kills_undo_chain() {
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "nihao".chars() {
        e.key(k(c));
    }
    match e.key(code_k(KEY_SPACE)) {
        Outcome::Commit(t) => assert_eq!(t, "你好"),
        other => panic!("expected Commit, got {:?}", other),
    }
    assert!(e.letters().is_empty());
    assert_eq!(e.undo_depth(), 0, "Commit 出口清链：上屏即撤销链死亡");
    // 上屏后退格 = Ignored 放行应用，不弹拼音、不消费栈
    assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Ignored);
    assert!(e.letters().is_empty(), "链死亡后无拼音弹回");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

/// #80 上屏 = 撤销链死亡：连选两词（你好、泥猴各自全覆盖立即上屏），
/// 每次 Commit 出口即清链——任意退格 = Ignored 放行应用，无 LIFO 弹回。
#[test]
fn two_word_commit_kills_undo_chain() {
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "nihao".chars() {
        e.key(k(c));
    }
    // 选词：nihao 被你好覆盖全部 → 立即上屏（fcitx5 语义）
    match e.key(code_k(KEY_SPACE)) {
        Outcome::Commit(t) => assert_eq!(t, "你好"),
        other => panic!("expected Commit, got {:?}", other),
    }
    for c in "nihou".chars() {
        e.key(k(c));
    }
    match e.key(code_k(KEY_SPACE)) {
        Outcome::Commit(t) => assert_eq!(t, "泥猴"),
        other => panic!("expected Commit, got {:?}", other),
    }
    assert_eq!(e.undo_depth(), 0, "链死亡：上屏即撤销终结");
    // 退格 1 与退格 2 都是 Ignored——链已死亡，应用自己删字
    for _ in 0..2 {
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Ignored);
        assert!(e.letters().is_empty(), "链死亡后无拼音弹回");
        assert_eq!(e.undo_depth(), 0);
    }
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

/// #77 撤销优先：pending 未释放时退格 = 弹词（消耗拼音 + 剩余整串还原），
/// 不删拼音；栈净后逐字删拼音，双空放行应用。（#80：上屏后链死亡与本路径无关——
/// 本测试全程 pending 未提交，弹词是 pending 内部的「未上屏幕词」撤回。）
#[test]
fn backspace_partial_selection_undoes_in_order() {
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "nihaoni".chars() {
        e.key(k(c));
    }
    // #81：选词入 pending（不上屏）
    assert!(matches!(e.key(code_k(KEY_SPACE)), Outcome::Consumed));
    assert_eq!(e.letters(), "ni");
    assert_eq!(e.preedit(), "你好ni");
    // 释放前退格 = 弹选词（纯内部，应用零删除；拼音弹回）
    assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Consumed);
    assert_eq!(e.letters(), "nihaoni", "弹词：消耗段 + 剩余 = 原串");
    assert_eq!(e.undo_depth(), 0, "弹词同序弹掉无效栈条目");
    // 栈净 → 删拼音逐字
    assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Consumed);
    assert_eq!(e.letters(), "nihaon");
    // 删净剩余（6 字）
    for _ in 0..6 {
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Consumed);
    }
    assert!(e.letters().is_empty());
    // 双空 = 空闲态，退格放行删应用字符
    assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Ignored);
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

/// #77 撤销优先：双拼部分选词弹回**键位串**（非拼音），不删除上屏字。
/// #80：Commit 出口清链后，上屏内容不可退格弹回——应用自己删字。
/// fcitx5 语义：空格提交 preedit 全部（你好 + 原样 zz 一起上屏）。
#[test]
fn shuangpin_backspace_after_commit_releases_to_app() {
    let (mut e, db, yaml) = engine_with_shuangpin_fixture();
    for c in "nihczz".chars() {
        e.key(k(c));
    }
    // 部分选词：nihc 被你好消耗，剩键位串 zz 入 pending
    assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
    assert_eq!(e.letters(), "zz", "部分选词剩键位串");
    assert_eq!(e.preedit(), "你好zz");
    // 释放前退格 = 弹词（键位串弹回，应用零删除）
    assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Consumed);
    assert_eq!(e.letters(), "nihczz", "弹词还原键位串");
    assert_eq!(e.undo_depth(), 0, "弹词同序清掉无效条目");
    // 再选：nihc 再被消耗，剩 zz → pending=[你好] letters="zz"
    assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
    assert_eq!(e.letters(), "zz");
    // 无候选可续 → 空格提交 preedit 全部：你好 + 原样 zz
    match e.key(code_k(KEY_SPACE)) {
        Outcome::Commit(t) => assert_eq!(t, "你好zz"),
        other => panic!("expected Commit, got {:?}", other),
    }
    assert_eq!(e.undo_depth(), 0, "Commit 出口清链：上屏即撤销链死亡");
    // 上屏后空闲退格 = Ignored：应用删字，引擎不弹词
    assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Ignored);
    assert_eq!(e.letters(), "", "链死亡后无拼音弹回");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

/// #77 推翻 #70：Esc 清组合（壳层 DEACTIVATE 合成 Esc 同此路径）=
/// 撤销链终结——面板关闭后退格放行应用，不弹旧拼音。
/// 前置必须是部分选词（全量选词已在 select_candidate 终止链路）。
#[test]
fn esc_clears_undo_stack() {
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "nihaoni".chars() {
        e.key(k(c));
    }
    // #81：选词入 pending；Esc 取消整句（含 pending），栈随组合清
    assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
    assert_eq!(e.letters(), "ni", "部分选词剩拼音，撤销链在栈上");
    assert_eq!(e.undo_depth(), 1);
    // Esc 清掉剩余组合——#77 起同步清栈（#70 旧契约推翻）
    assert_eq!(e.key(code_k(KEY_ESC)), Outcome::Consumed);
    assert!(e.preedit().is_empty());
    assert_eq!(e.undo_depth(), 0, "组合清 = 撤销链终结");
    // 空闲态：退格放行应用，不弹旧拼音
    assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Ignored);
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

/// #80 上屏 = 撤销链死亡：C-h 删空剩余拼音不经 Commit 出口，链保留；
/// 空格释放整段后链死亡（#79「删空后仍可弹上屏字」语义作废），
/// 此后退格 = Ignored 放行应用。
#[test]
fn ctrl_h_delete_to_empty_then_commit_kills_chain() {
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "nihaoni".chars() {
        e.key(k(c));
    }
    // #81：选词入 pending（不上屏）
    assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
    assert_eq!(e.letters(), "ni");
    assert_eq!(e.undo_depth(), 1);
    // C-h 逐字删（光标在尾）：删空不清链（#77：链在组合非空时存活）
    assert_eq!(e.key(ctrl_h_k()), Outcome::Consumed);
    assert_eq!(e.letters(), "n");
    assert_eq!(e.key(ctrl_h_k()), Outcome::Consumed);
    assert!(e.letters().is_empty());
    assert_eq!(e.undo_depth(), 1, "删空不清链（链在组合非空时存活）");
    // 空格释放整段 = Commit 出口，清链：上屏即链死
    match e.key(code_k(KEY_SPACE)) {
        Outcome::Commit(t) => assert_eq!(t, "你好"),
        other => panic!("expected 释放 Commit, got {:?}", other),
    }
    assert_eq!(e.undo_depth(), 0, "Commit 出口清链：上屏即撤销链死亡");
    // 释放后退格 = Ignored 放行应用，不再弹词
    assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Ignored);
    assert!(e.letters().is_empty(), "链死亡后无拼音弹回");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

/// Shift-flush 路径：letters 非空时 Shift 走 Commit(**原字母**) + 清组合
/// + 切英文——输入串随原样上屏结束，词级撤销链随 burst 终止；
/// 退格放行给应用（英文模式同样 Ignored），不再弹回旧选词拼音
/// （v0.19.68 前语义是保留，3eb8279 的 `shift_flush_keeps_undo_stack`
/// 语义反转）。
#[test]
fn shift_flush_ends_undo_chain() {
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "nihaoni".chars() {
        e.key(k(c));
    }
    // #81：选词入 pending
    assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
    assert_eq!(e.letters(), "ni", "部分选词剩拼音");
    assert_eq!(e.undo_depth(), 1);
    // 剩余组合在、直接 Shift：pending + 原字母整段上屏 + 清组合 + 切英文，burst 结束
    match e.key(shift_k(KEY_LEFTSHIFT)) {
        Outcome::Commit(t) => assert_eq!(t, "你好ni", "shift-flush 连 pending 冲刷"),
        other => panic!("expected Commit(你好ni), got {:?}", other),
    }
    assert!(!e.chinese(), "Shift-flush 切英文（输入串结束）");
    assert!(e.letters().is_empty());
    assert_eq!(
        e.undo_depth(),
        0,
        "shift-flush 上屏即输入串结束，撤销链终止"
    );
    // 退格放行，不再弹旧词（英文模式同样 Ignored）
    assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Ignored);
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

/// 标点上屏 = 输入串结束：组合空后标点直接上屏；空闲退格放行应用删全角。
#[test]
fn punct_commit_ends_undo_chain() {
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "nihao".chars() {
        e.key(k(c));
    }
    // 空格：候选覆盖全部输入 → 立即上屏（#80：Commit 出口即清链）
    match e.key(code_k(KEY_SPACE)) {
        Outcome::Commit(t) => assert_eq!(t, "你好"),
        other => panic!("expected Commit, got {:?}", other),
    }
    assert_eq!(e.undo_depth(), 0, "上屏即链死");
    // 组合已空：标点直接上屏（原样上屏 = 输入串结束，撤销链终止）
    match e.key(k('.')) {
        Outcome::Commit(t) => assert_eq!(t, "。"),
        other => panic!("expected Commit, got {:?}", other),
    }
    assert_eq!(e.undo_depth(), 0, "标点 Commit 即输入串结束，撤销链终止");
    // 空闲退格 = Ignored 放行应用删全角字符
    assert_eq!(
        e.key(code_k(KEY_BACKSPACE)),
        Outcome::Ignored,
        "组合空后退格放行应用删全角"
    );
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}
/// #96 起 burst 结束（选词上屏即输入串结束）——撤销链立即死。
/// 轻点 Shift = 翻转中/英模式（头段后两段不变）。
#[test]
fn english_toggle_after_burst_end() {
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "nihao".chars() {
        e.key(k(c));
    }
    // 全覆盖选词 = 立即上屏（fcitx5 语义）；#96：上屏即撤销链死亡
    match e.key(code_k(KEY_SPACE)) {
        Outcome::Commit(t) => assert_eq!(t, "你好"),
        other => panic!("expected Commit, got {:?}", other),
    }
    // 空闲退格放行应用，不再弹词还原拼音（#79/#80 跨上屏撤销作废）
    assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Ignored);
    assert!(e.letters().is_empty());
    assert_eq!(e.undo_depth(), 0, "上屏即链死（#96）");
    // 空 letters：轻点 Shift 切英文，字母/退格一律直通
    assert_eq!(e.key(shift_k(KEY_LEFTSHIFT)), Outcome::Consumed);
    assert!(!e.chinese());
    assert_eq!(e.key(k('n')), Outcome::Ignored, "英文模式不组拼音");
    assert_eq!(
        e.key(code_k(KEY_BACKSPACE)),
        Outcome::Ignored,
        "英文模式退格放行给应用"
    );
    // 切回中文：退格放行不弹旧词，拼音照常
    assert_eq!(e.key(shift_k(KEY_LEFTSHIFT)), Outcome::Consumed);
    assert!(e.chinese());
    assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Ignored);
    assert_eq!(e.key(k('n')), Outcome::Consumed, "中文模式照常吃拼音");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

/// 空闲轻点 Shift = 翻转中/英模式（壳层把轻点裁决成 Toggle 后补交这一次
/// Shift）：中→英→中逐次断言。
#[test]
fn shift_toggles_to_english() {
    let (mut e, db, yaml) = engine_with_fixture();
    assert!(e.chinese());
    assert_eq!(e.key(shift_k(KEY_LEFTSHIFT)), Outcome::Consumed);
    assert!(!e.chinese());
    assert_eq!(e.key(shift_k(KEY_LEFTSHIFT)), Outcome::Consumed);
    assert!(e.chinese());
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

/// 英文模式字母/数字/空格/标点全 Ignored 直通，preedit 恒空；
/// 再轻点切回中文后拼音照常进组合。
#[test]
fn english_mode_passes_keys_through() {
    let (mut e, db, yaml) = engine_with_fixture();
    assert_eq!(e.key(shift_k(KEY_LEFTSHIFT)), Outcome::Consumed); // → english
    assert!(!e.chinese());
    assert_eq!(e.key(k('a')), Outcome::Ignored);
    assert_eq!(e.key(k('1')), Outcome::Ignored);
    assert!(e.preedit().is_empty());
    assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Ignored);
    assert_eq!(e.key(k('.')), Outcome::Ignored);
    // 切回中文：组合照常
    assert_eq!(e.key(shift_k(KEY_LEFTSHIFT)), Outcome::Consumed);
    assert!(e.chinese());
    assert_eq!(e.key(k('n')), Outcome::Consumed);
    assert_eq!(e.preedit(), "n");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

/// Shift-flush = 原字母整串上屏 + 清组合 + 切英文；之后字母直通，
/// 再轻点 Shift 切回中文（拼音照常）
#[test]
fn shift_with_active_composition_commits_raw_and_switches() {
    let (mut e, db, yaml) = engine_with_fixture();
    e.key(k('n'));
    let mut alt_n = k('n');
    alt_n.alt = true;
    assert_eq!(e.key(alt_n), Outcome::Ignored);
    assert_eq!(e.preedit(), "n");
    e.key(k('h'));
    assert!(e.chinese());
    assert_eq!(e.key(shift_k(KEY_LEFTSHIFT)), Outcome::Commit("nh".into()));
    assert!(!e.chinese(), "冲刷后切英文");
    assert!(e.preedit().is_empty());
    assert_eq!(e.key(k('a')), Outcome::Ignored, "英文模式字母直通");
    // 切回中文：拼音照常进组合
    assert_eq!(e.key(shift_k(KEY_LEFTSHIFT)), Outcome::Consumed);
    assert!(e.chinese());
    assert_eq!(e.key(k('a')), Outcome::Consumed);
    assert_eq!(e.preedit(), "a");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn complete_pinyin_finds_exact_candidate() {
    // "nihao" → 完整双字拼音，应命中 "你好"。
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "nihao".chars() {
        e.key(k(c));
    }
    let cs: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
    assert!(
        cs.contains(&"你好".to_string()),
        "completed pinyin 'nihao' should surface 你好, got {:?}",
        cs
    );
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn single_letter_pinyin_returns_candidates_starting_with_it() {
    // "a" 单独成音节 → tail="a", reading=[] → LIKE 'a%' 命中 "安" / "啊啊"。
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "a".chars() {
        e.key(k(c));
    }
    let cs: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
    assert!(
        cs.contains(&"安".to_string()),
        "a should match 安, got {:?}",
        cs
    );
    assert!(
        cs.contains(&"啊啊".to_string()),
        "a should match 啊啊, got {:?}",
        cs
    );
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn invalid_pinyin_keeps_preedit_but_no_candidates() {
    // "xq" → 整串 segment 为空，找合法 head → "x" 也是单字音节 → tail="q"。
    // dict 中无 "xq..." 前缀 → 候选空，preedit 仍保留。
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "xq".chars() {
        e.key(k(c));
    }
    assert_eq!(e.preedit(), "xq");
    assert!(e.candidates().is_empty(), "xq should have no candidates");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

// --- M5: 翻页 + 页内选词 ---
fn fixture_many_ni() -> (std::path::PathBuf, std::path::PathBuf) {
    let db = tmp_db("page");
    let yaml = std::env::temp_dir().join(format!("kime_page_{}.yaml", std::process::id()));
    std::fs::write(&yaml, "").unwrap();
    // raw INSERT 前必须建表：Dict::open 负责 schema，缺了 5 个 page_* 测试在 base 上即崩。
    let _dict = Dict::open(&db).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    for i in 0..25 {
        conn.execute(
            "INSERT OR IGNORE INTO phrase(pinyin, text, freq, abbrev, user) VALUES (?, ?, ?, 'n', 0)",
            rusqlite::params![format!("ni'{}", i), format!("词{}", i), 1000 - i as i64],
        )
        .unwrap();
    }
    drop(conn);
    (db, yaml)
}

#[test]
fn page_navigation_clamps_and_page_local_digits() {
    let (db, yaml) = fixture_many_ni();
    let dict = Dict::open(&db).unwrap();
    let mut e = Engine::new(dict, Config::default());
    for c in "ni".chars() {
        e.key(k(c));
    }
    assert!(e.candidates().len() >= 20);
    assert_eq!(e.page(), (0, 10));
    // 上一页越界钳位
    assert_eq!(e.key(code_k(KEY_MINUS)), Outcome::Consumed);
    assert_eq!(e.page(), (0, 10));
    // 下一页
    assert_eq!(e.key(code_k(KEY_EQUAL)), Outcome::Consumed);
    assert_eq!(e.page(), (1, 10));
    // 页内数字 2 → 全局第 12 个候选（覆盖全部输入 = 立即上屏）
    let out = e.key(k('2'));
    match out {
        Outcome::Commit(t) => assert_eq!(t, "词11"),
        other => panic!("expected Commit, got {:?}", other),
    }
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn page_digit_beyond_page_is_ignored() {
    let (db, yaml) = fixture_many_ni();
    let dict = Dict::open(&db).unwrap();
    let mut e = Engine::new(dict, Config::default());
    for c in "ni".chars() {
        e.key(k(c));
    }
    // 第 0 页只有 10 个候选：数字 9 有效，但先翻到最后一页（25 条 → 第 2 页只有 5 个）
    e.key(code_k(KEY_EQUAL));
    e.key(code_k(KEY_EQUAL));
    let last_page = e.page();
    assert_eq!(last_page, (2, 10));
    // 页内索引 8 超过最后页剩余 5 个 → Ignored
    assert_eq!(e.key(k('9')), Outcome::Ignored);
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn page_resets_on_commit_and_esc() {
    let (db, yaml) = fixture_many_ni();
    let dict = Dict::open(&db).unwrap();
    let mut e = Engine::new(dict, Config::default());
    for c in "ni".chars() {
        e.key(k(c));
    }
    e.key(code_k(KEY_EQUAL));
    assert_eq!(e.page(), (1, 10));
    e.key(code_k(KEY_ESC));
    assert_eq!(e.page(), (0, 10));
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn paging_without_candidates_is_ignored() {
    let (db, yaml) = fixture_many_ni();
    let dict = Dict::open(&db).unwrap();
    let mut e = Engine::new(dict, Config::default());
    assert_eq!(e.key(code_k(KEY_EQUAL)), Outcome::Ignored);
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

// --- M5: 模糊音 ---
const FUZZY_YAML: &str =
    "---\n...\n里\tli\t800\n凉\tliang\t700\n饭\tfan\t900\n翻\tfang\t600\n你\tni\t5000\n";

#[test]
fn fuzzy_empty_config_is_baseline() {
    let db = tmp_db("fz0");
    let yaml = std::env::temp_dir().join("kime_fz0.yaml");
    std::fs::write(&yaml, FUZZY_YAML).unwrap();
    let mut dict = Dict::open(&db).unwrap();
    dict.import(&yaml).unwrap();
    let mut e = Engine::new(dict, Config::default());
    for c in "ni".chars() {
        e.key(k(c));
    }
    let texts: Vec<&str> = e.candidates().iter().map(|c| c.text.as_str()).collect();
    assert!(texts.contains(&"你"));
    assert!(!texts.contains(&"里"), "无模糊配置时不应命中 li 词");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn fuzzy_initial_n_to_l_matches_li_words() {
    let db = tmp_db("fz1");
    let yaml = std::env::temp_dir().join("kime_fz1.yaml");
    std::fs::write(&yaml, FUZZY_YAML).unwrap();
    let mut dict = Dict::open(&db).unwrap();
    dict.import(&yaml).unwrap();
    let cfg = Config {
        shuangpin: None,
        fuzzy: vec!["n=l".to_string()],
        ..Config::default()
    };
    let mut e = Engine::new(dict, cfg);
    for c in "ni".chars() {
        e.key(k(c));
    }
    let texts: Vec<&str> = e.candidates().iter().map(|c| c.text.as_str()).collect();
    assert!(texts.contains(&"你"));
    assert!(texts.contains(&"里"), "fuzzy n=l 应命中 li 词");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn fuzzy_final_an_to_ang_matches_fang() {
    let db = tmp_db("fz2");
    let yaml = std::env::temp_dir().join("kime_fz2.yaml");
    std::fs::write(&yaml, FUZZY_YAML).unwrap();
    let mut dict = Dict::open(&db).unwrap();
    dict.import(&yaml).unwrap();
    let cfg = Config {
        shuangpin: None,
        fuzzy: vec!["an=ang".to_string()],
        ..Config::default()
    };
    let mut e = Engine::new(dict, cfg);
    for c in "fan".chars() {
        e.key(k(c));
    }
    let texts: Vec<&str> = e.candidates().iter().map(|c| c.text.as_str()).collect();
    assert!(texts.contains(&"饭"));
    assert!(texts.contains(&"翻"), "fuzzy an=ang 应命中 fang 词");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn fuzzy_invalid_entries_ignored() {
    let db = tmp_db("fz3");
    let yaml = std::env::temp_dir().join("kime_fz3.yaml");
    std::fs::write(&yaml, FUZZY_YAML).unwrap();
    let mut dict = Dict::open(&db).unwrap();
    dict.import(&yaml).unwrap();
    let cfg = Config {
        shuangpin: None,
        fuzzy: vec!["xyz".to_string(), "=x".to_string(), "x=".to_string()],
        ..Config::default()
    };
    let mut e = Engine::new(dict, cfg);
    for c in "ni".chars() {
        e.key(k(c));
    }
    assert!(!e.candidates().is_empty(), "非法配置不影响正常查询");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn fuzzy_dedupe_across_paths() {
    let db = tmp_db("fz4");
    let yaml = std::env::temp_dir().join("kime_fz4.yaml");
    // 同一文本同时有 ni / li 两行（前缀互不包含）→ 主路与模糊路都命中但只出现一次
    std::fs::write(&yaml, "---\n...\n泥\tni\t600\n泥\tli\t500\n你\tni\t900\n").unwrap();
    let mut dict = Dict::open(&db).unwrap();
    dict.import(&yaml).unwrap();
    let cfg = Config {
        shuangpin: None,
        fuzzy: vec!["n=l".to_string()],
        ..Config::default()
    };
    let mut e = Engine::new(dict, cfg);
    for c in "ni".chars() {
        e.key(k(c));
    }
    let count = e.candidates().iter().filter(|c| c.text == "泥").count();
    assert_eq!(count, 1, "去重：同一文本只出现一次");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn llm_merge_adds_candidates() {
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "nihao".chars() {
        e.key(k(c));
    }
    let original_len = e.candidates().len();

    let ai = vec![Candidate {
        text: "你好世界".to_string(),
        pinyin: "ni'hao".to_string(),
        freq: 1,
        eff: 1,
        ai: true,
    }];
    e.merge_ai(ai);

    assert_eq!(e.candidates().len(), original_len + 1);
    assert!(e.candidates().iter().any(|c| c.text == "你好世界" && c.ai));
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn llm_dedupe_existing() {
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "nihao".chars() {
        e.key(k(c));
    }
    let original_len = e.candidates().len();

    // Merge a candidate that already exists
    let ai = vec![Candidate {
        text: "你好".to_string(),
        pinyin: "ni'hao".to_string(),
        freq: 1,
        eff: 1,
        ai: true,
    }];
    e.merge_ai(ai);

    // Should not add duplicate
    assert_eq!(e.candidates().len(), original_len);
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn test_viterbi_sentence_first_candidate() {
    let db = tmp_db("viterbi_engine");
    let yaml = std::env::temp_dir().join("kime_viterbi_engine.yaml");
    std::fs::write(
        &yaml,
        "---\n...\n你好\tni hao\t5000\n世界\tshi jie\t4000\n你\tni\t1000\n好\thao\t1000\n",
    )
    .unwrap();
    let mut dict = Dict::open(&db).unwrap();
    dict.import(&yaml).unwrap();
    let cfg = Config {
        shuangpin: None,
        ..Config::default()
    };
    let mut e = Engine::new(dict, cfg);
    for c in "nihaoshijie".chars() {
        e.key(k(c));
    }
    let cands = e.candidates();
    assert!(!cands.is_empty());
    // 第一候选应当是由 Viterbi 最优路径合成的连贯整句「你好世界」
    assert_eq!(cands[0].text, "你好世界");

    let outcome = e.key(Key {
        ch: None,
        code: KEY_SPACE,
        shift: false,
        ctrl: false,
        alt: false,
    });
    match outcome {
        Outcome::Commit(text) => assert_eq!(text, "你好世界"),
        other => panic!("expected Commit, got {:?}", other),
    }
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn digit_0_selects_tenth_candidate() {
    // 构造 12 个候选，翻到第 1 页后按 0 → 选第 10 个（全局索引 9）
    let (db, yaml) = fixture_many_ni();
    let dict = Dict::open(&db).unwrap();
    let cfg = Config {
        page_size: 10,
        shuangpin: None,
        ..Config::default()
    };
    let mut e = Engine::new(dict, cfg);
    for c in "ni".chars() {
        e.key(k(c));
    }
    // 翻到第 2 页
    e.key(code_k(KEY_EQUAL));
    assert_eq!(e.page().0, 1);
    // 按 0 → 选第 10 个候选（覆盖全部输入 = 立即上屏；全局索引 19）
    let outcome = e.key(k('0'));
    match outcome {
        Outcome::Commit(t) => assert_eq!(t, "词19"),
        other => panic!("expected Commit, got {:?}", other),
    }
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

/// #81 空格修页：翻到第 2 页后按空格 = 选**当前页**首候选（全局
/// 索引 10）入 pending，不再写死第一页首候选。
#[test]
fn space_selects_current_page_top_after_paging() {
    let (db, yaml) = fixture_many_ni();
    let dict = Dict::open(&db).unwrap();
    let cfg = Config {
        page_size: 10,
        shuangpin: None,
        ..Config::default()
    };
    let mut e = Engine::new(dict, cfg);
    for c in "ni".chars() {
        e.key(k(c));
    }
    e.key(code_k(KEY_EQUAL)); // 翻到第 2 页
    assert_eq!(e.page().0, 1);
    assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Commit("词10".into()));
    assert!(
        e.preedit().is_empty(),
        "候选覆盖全部输入 = 立即上屏（fcitx5 语义）",
    );
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

/// #81 Esc 取消：选词入 pending 后按 Esc = 整句不上屏（应用零提交），
/// pending 与剩余拼音全清；已释放词不受影响。
#[test]
fn esc_cancels_pending_without_commit() {
    let (mut e, db, yaml) = engine_with_fixture();
    for c in "nihaoni".chars() {
        e.key(k(c));
    }
    assert_eq!(
        e.key(code_k(KEY_SPACE)),
        Outcome::Consumed,
        "选词入 pending"
    );
    assert_eq!(e.preedit(), "你好ni");
    assert_eq!(e.key(code_k(KEY_ESC)), Outcome::Consumed, "Esc 取消");
    assert!(e.letters().is_empty());
    assert!(e.preedit().is_empty(), "显示串全清，无内容上屏");
    assert!(e.candidates().is_empty());
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn ctrl_dot_toggles_punct_mode() {
    let (mut e, db, yaml) = engine_with_fixture();
    // 默认中文标点模式
    assert!(matches!(e.punct_mode(), PunctMode::Chinese));
    // Ctrl+. 切换到英文
    let outcome = e.key(Key {
        ch: Some('.'),
        code: 0,
        shift: false,
        ctrl: true,
        alt: false,
    });
    assert_eq!(outcome, Outcome::Consumed);
    assert!(matches!(e.punct_mode(), PunctMode::English));
    // 再按回来
    let outcome = e.key(Key {
        ch: Some('.'),
        code: 0,
        shift: false,
        ctrl: true,
        alt: false,
    });
    assert_eq!(outcome, Outcome::Consumed);
    assert!(matches!(e.punct_mode(), PunctMode::Chinese));
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn english_punct_passes_through() {
    let (mut e, db, yaml) = engine_with_fixture();
    // 切换到英文标点模式
    e.set_punct_mode(PunctMode::English);
    // 输入逗号 → 应原样输出 "," 而非 "，"
    let outcome = e.key(Key {
        ch: Some(','),
        code: 0,
        shift: false,
        ctrl: false,
        alt: false,
    });
    assert_eq!(outcome, Outcome::Commit(",".to_string()));
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

fn engine_with_sp_sentence_fixture() -> (Engine, std::path::PathBuf, std::path::PathBuf) {
    let db = tmp_db("sp_sentence");
    let yaml = fixture_yaml_path();
    let _dict = Dict::open(&db).expect("open dict");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "INSERT OR REPLACE INTO phrase(pinyin, text, freq, abbrev, user) VALUES
           ('ni', '你', 5000, 'n', 0),
           ('ni''qu', '你骑', 3000, 'nq', 0),
           ('qu', '裙', 2000, 'q', 0),
           ('que', '却', 1500, 'qu', 0),
           ('quan', '全', 300, 'q', 0),
           ('ni''quan', '你全', 50, 'nq', 0),
           ('quan''que', '全却', 200000, 'qq', 0),
           ('ni''quan''que''ne', '你全却呢', 3000, 'nq', 0),
           ('ni''quan''que''neng', '你全却能', 1, 'nq', 0),
           ('a', '安', 8000, 'a', 0);
        ",
    )
    .unwrap();
    drop(conn);
    let dict = Dict::open(&db).expect("reopen dict for lookups");
    let config = Config {
        dict_path: db.to_string_lossy().to_string(),
        shuangpin: Some(Scheme::Xiaohe),
        ..Config::default()
    };
    let engine = Engine::new(dict, config);
    (engine, db, yaml)
}

#[test]
fn sp_odd_pending_keeps_sentence_in_pool() {
    // 双拼 niqrqtn = 键对 n+i=ni, q+r=quan, q+t=que, 半截 n（奇数 pending）。
    // 主查补开区间 [ni'quan'que'n, ...) = 你全却呢(3000)/你全却能(1)；
    // 全覆盖 Viterbi 句 = 你 + 全却 → 「你全却」（文本与 L1/L2 词互异）。
    // 修前句 pinyin（完整音节 join）匹配不到含 pending 的 sp_joined 被挂尾；
    // 修后按整音节 join 落位，句恒插层一精确块之后（merge 保护段保证
    // 首音节单字交错后句仍居首）——fcitx5 组句语义：长句恒在第一。
    let (mut e, db, yaml) = engine_with_sp_sentence_fixture();
    for c in "niqrqtn".chars() {
        e.key(k(c));
    }
    let cands = e.candidates();
    assert!(!cands.is_empty(), "niqrqtn 应出候选");
    let idx = |t: &str| cands.iter().position(|c| c.text == t).unwrap_or(usize::MAX);
    assert!(
        idx("你全却") != usize::MAX,
        "全覆盖句候选「你全却」必须在列表内：{cands:?}"
    );
    assert!(
        idx("你全却") < idx("你全却呢") && idx("你全却") < idx("你全却能"),
        "句须恒在层一精确块之后、其余候选之前（fcitx5 组句语义），\
         不许按词条分数被压到中间：{cands:?}"
    );
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

fn engine_with_full_pinyin_pending_fixture() -> (Engine, std::path::PathBuf, std::path::PathBuf) {
    let db = tmp_db("fullpy");
    let yaml = fixture_yaml_path();
    let _dict = Dict::open(&db).expect("open dict");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "INSERT OR REPLACE INTO phrase(pinyin, text, freq, abbrev, user) VALUES
           ('shen', '神', 90, 'sh', 0),
           ('shen''me', '什么', 12000, 'sm', 0),
           ('shen''me''yang''de', '什么样的', 10, 'sm', 0);
        ",
    )
    .unwrap();
    drop(conn);
    let dict = Dict::open(&db).expect("reopen dict for lookups");
    let config = Config {
        shuangpin: None,
        ..Config::default()
    };
    let engine = Engine::new(dict, config);
    (engine, db, yaml)
}

#[test]
fn full_pinyin_pending_tail_keeps_sentence() {
    // 全拼 shenmey = 完整音节 [shen,me] + 半截尾巴 y。is_syllable("y")==false
    // → segment 整串切不出任何路径 → 修前句级联想被 segs.first()==None 短路，
    // Viterbi 从不跑，句「什么」整个消失（主查只剩带尾前缀词条「什么样的」）
    // ——用户实测「偶数键位长句在第一、奇数键位掉队/消失」。修后退回
    // fallback 切分的完整音节组句：句恒随已敲完的音节计算、落层一之后
    // （fcitx5 语义），半截尾巴不影响句位。
    let (mut e, db, yaml) = engine_with_full_pinyin_pending_fixture();
    for c in "shenmey".chars() {
        e.key(k(c));
    }
    let cands = e.candidates();
    let idx = |t: &str| cands.iter().position(|c| c.text == t).unwrap_or(usize::MAX);
    assert!(
        idx("什么") != usize::MAX,
        "句候选「什么」必须在列表内（修前整个消失）：{cands:?}"
    );
    assert_eq!(
        idx("什么"),
        0,
        "句须恒在层一之后、其余候选之前（fcitx5 组句语义）：{cands:?}"
    );
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn full_pinyin_complete_tail_keeps_sentence_first() {
    // 回归钉：完整尾巴（shenme，tail="me" 是合法音节）走 segs.first() 老路，
    // 句「什么」与词条文本去重后词条本身居首——修法不得改变此行为。
    let (mut e, db, yaml) = engine_with_full_pinyin_pending_fixture();
    for c in "shenme".chars() {
        e.key(k(c));
    }
    let cands = e.candidates();
    let idx = |t: &str| cands.iter().position(|c| c.text == t).unwrap_or(usize::MAX);
    assert_eq!(
        idx("什么"),
        0,
        "完整输入的句/词条「什么」必须居首：{cands:?}"
    );
    assert!(
        idx("什么样的") != usize::MAX,
        "补全词条「什么样的」必须在列表内：{cands:?}"
    );
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

#[test]
fn space_flushes_leftover_letters_after_all_selected() {
    // 双拼 aaj = [a] + 半截 j（xiaohe aa=a）：选「安」后残留字母 j
    // （sp 选词按键位消耗 2 键/音节，残 1 键）。#81 语义：第一下空格只释放
    // pending（安），残留 j 留在组合；第二下空格（无候选、pending 已空、
    // letters 非空）原样上屏 j 收尾——修前第二下走 Ignored，残串永远挂
    // preedit 不输出（用户反馈「全部候选选完时选中没有上屏」）。
    let (mut e, db, yaml) = engine_with_sp_sentence_fixture();
    for c in "aaj".chars() {
        e.key(k(c));
    }
    let texts: Vec<&str> = e.candidates().iter().map(|c| c.text.as_str()).collect();
    assert!(texts.contains(&"安"), "aaj 须可选到安，实际 {texts:?}");
    // 选到列表为空（安入 pending，不上屏；j 部分覆盖留组合）
    while !e.candidates().is_empty() {
        assert_eq!(
            e.key(code_k(KEY_SPACE)),
            Outcome::Consumed,
            "选词入 pending"
        );
    }
    assert_eq!(e.letters(), "j", "残留字母须留在组合");
    // 无候选可续 → 空格提交 preedit 全部（fcitx5 语义）：安 + 原样 j
    match e.key(code_k(KEY_SPACE)) {
        Outcome::Commit(t) => assert_eq!(t, "安j", "preedit 全部上屏，实际 {t:?}"),
        other => panic!("空格应提交 preedit 全部，实际 {other:?}"),
    }
    assert!(
        e.letters().is_empty() && e.preedit().is_empty(),
        "组合须结束"
    );
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}
