//! 切应用焦点时组合必须作废：Deactivate 清引擎组合 + 拉平旧应用 preedit。
//!
//! 壳层 `AppState::clear_composition` 的引擎半边 = 喂一个裸 ESC（与 XIM 前端
//! `handle_unset_focus` 同手法：`Engine::clear_composition` 私有，ESC 路径是
//! 公开的等价全量清组合入口，且不动中英模式）。这里钉住该入口契约：清完后
//! preedit/候选/页码全空，新应用的第一个键从空组合起，不接续旧拼音。
//!
//! Wayland 协议半边（`set_preedit_string("") + commit`）在主进程里，集成测试
//! 无法拉起真实合成器；它与现有 `deliver_commit` 同形，不另设测试。

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::engine::{Engine, Outcome};
use platform_wayland::route::shell_key;
/// evdev 码（QWERTY）：字母路径引擎只认 `ch`，码仅用于壳层记账。
const KEY_N: u32 = 49;
const KEY_I: u32 = 23;
const KEY_ESC: u32 = 1;

/// 词库 fixture：`...` 分隔线 + 词\t拼音\t词频。
const CN: &str = "...\n你好\tni hao\t9000000\n拟好\tni hao\t4000000\n";

fn test_engine() -> Engine {
    let dir = std::env::temp_dir().join(format!("kime_deactivate_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cn = dir.join("cn.tsv");
    std::fs::write(&cn, CN).unwrap();
    let mut dict = Dict::open(dir.join("dict.sqlite3")).unwrap();
    dict.import(&cn).unwrap();
    Engine::new(dict, Config::default())
}

#[test]
fn esc_clears_composition_like_deactivate() {
    let mut engine = test_engine();
    for (code, ch) in [(KEY_N, 'n'), (KEY_I, 'i')] {
        assert_eq!(
            engine.key(shell_key(code, Some(ch), false, false)),
            Outcome::Consumed
        );
    }
    assert!(!engine.preedit().is_empty(), "先打出在途组合");
    assert!(!engine.candidates().is_empty(), "组合应带候选");

    // Deactivate 的引擎半边
    assert_eq!(
        engine.key(shell_key(KEY_ESC, None, false, false)),
        Outcome::Consumed
    );

    assert!(engine.preedit().is_empty(), "preedit 必须清空");
    assert!(engine.candidates().is_empty(), "候选必须清空");
    assert_eq!(engine.page(), (0, 10), "页码归位");

    // 新应用的第一个键从空组合起，不接续旧拼音
    assert_eq!(
        engine.key(shell_key(KEY_N, Some('n'), false, false)),
        Outcome::Consumed
    );
    assert_eq!(engine.preedit(), "n");
}
