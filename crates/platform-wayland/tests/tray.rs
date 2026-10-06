//! 托盘状态机行为测试，经 platform_wayland::tray 公开 API。

use platform_wayland::tray::TrayIconManager;

#[test]
fn test_tray_manager_toggle() {
    let manager = TrayIconManager::new(true);
    assert!(manager.is_chinese());
    assert!(!manager.toggle());
    assert!(!manager.is_chinese());
    assert!(manager.toggle());
    assert!(manager.is_chinese());
}

#[test]
fn test_tray_manager_set() {
    let manager = TrayIconManager::new(true);
    manager.set_chinese(false);
    assert!(!manager.is_chinese());
    manager.set_chinese(true);
    assert!(manager.is_chinese());
}
