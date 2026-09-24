//! 模式徽标开关的纯函数测试。
//!
//! 刻意只测 `badge_enabled_from`，不调用 `badge_enabled()`、不写进程环境变量：
//! 集成测试的多个 #[test] 跑在同一进程的多个线程上，环境变量是进程级共享状态，
//! 任何 set_var/remove_var 都会与其他测试用例互相污染，结果不确定。

use platform_wayland::mode_badge::badge_enabled_from;

#[test]
fn unset_is_enabled() {
    assert!(badge_enabled_from(None));
}

#[test]
fn off_spellings_are_disabled() {
    for v in ["0", "false", "off", "no"] {
        assert!(!badge_enabled_from(Some(v)), "{v} 应关闭");
    }
}

#[test]
fn off_spellings_are_case_insensitive() {
    for v in ["OFF", "False", "NO"] {
        assert!(!badge_enabled_from(Some(v)), "{v} 应关闭");
    }
}

#[test]
fn off_spellings_allow_surrounding_whitespace() {
    for v in [" 0 ", "\tfalse\n", "  OFF  "] {
        assert!(!badge_enabled_from(Some(v)), "{v:?} 应关闭");
    }
}

#[test]
fn empty_string_is_enabled() {
    assert!(badge_enabled_from(Some("")));
    assert!(badge_enabled_from(Some("   ")));
}

#[test]
fn unrecognized_values_are_enabled() {
    for v in ["maybe", "2", "-1", "yes please"] {
        assert!(badge_enabled_from(Some(v)), "{v} 应开启");
    }
}

#[test]
fn on_spellings_are_enabled() {
    for v in ["1", "true", "on", "yes", "YES", " On "] {
        assert!(badge_enabled_from(Some(v)), "{v} 应开启");
    }
}
