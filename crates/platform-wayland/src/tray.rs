//! 托盘图标与状态指示
//!
//! 使用 StatusNotifierItem (appindicator) 协议在系统托盘显示当前中英模式。
//! 点击托盘图标可切换中英文模式。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// 托盘图标管理器
pub struct TrayIconManager {
    is_chinese: Arc<AtomicBool>,
}

impl TrayIconManager {
    pub fn new(is_chinese: bool) -> Self {
        Self {
            is_chinese: Arc::new(AtomicBool::new(is_chinese)),
        }
    }

    pub fn is_chinese(&self) -> bool {
        self.is_chinese.load(Ordering::Relaxed)
    }

    pub fn set_chinese(&self, chinese: bool) {
        self.is_chinese.store(chinese, Ordering::Relaxed);
    }

    pub fn toggle(&self) -> bool {
        let current = self.is_chinese.load(Ordering::Relaxed);
        let new = !current;
        self.is_chinese.store(new, Ordering::Relaxed);
        new
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
