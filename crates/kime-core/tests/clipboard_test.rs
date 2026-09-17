//! 剪贴板候选的公共 API 测试：只走消费者视角（candidates/presets），
//! 不碰私有字段（本仓规范：测试放同层 tests/ 目录）。

use kime_core::clipboard::{ClipStore, HISTORY_CAP, PRESET_MAX_CHARS};
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_dir(tag: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("kime-clip-{tag}-{}-{nanos}", std::process::id()))
}

#[test]
fn push_dedups_and_moves_to_front() {
    let mut s = ClipStore::new();
    s.push("a", 1);
    s.push("b", 2);
    s.push("a", 3);
    let texts: Vec<&str> = s.candidates().iter().map(|e| e.text.as_str()).collect();
    assert_eq!(texts, ["a", "b"]); // 去重后置顶
    assert_eq!(s.candidates()[0].ts, 3); // 时间戳刷新
}

#[test]
fn push_skips_blank() {
    let mut s = ClipStore::new();
    s.push("  ", 1);
    s.push("", 2);
    s.push("\n\t", 3);
    assert!(s.candidates().is_empty());
}

#[test]
fn history_is_bounded_newest_first() {
    let mut s = ClipStore::new();
    for i in 0..(HISTORY_CAP as u64 + 10) {
        s.push(&format!("t{i}"), i);
    }
    let cands = s.candidates();
    assert_eq!(cands.len(), HISTORY_CAP);
    assert_eq!(cands[0].text, "t73"); // 最新置顶
    assert_eq!(cands[cands.len() - 1].text, "t10"); // 最老的 10 条被挤出
}

#[test]
fn presets_load_from_deskctl_dir_sorted_by_name() {
    let root = unique_dir("presets");
    std::fs::create_dir_all(root.join("models")).unwrap();
    std::fs::create_dir_all(root.join("paths")).unwrap();
    std::fs::write(root.join("models/qwen"), "qwen/model\n").unwrap();
    std::fs::write(root.join("models/gpt"), "gpt-5.5").unwrap();
    std::fs::write(root.join("paths/repo"), "~/proj").unwrap();

    let mut s = ClipStore::new();
    s.load_presets(&root);
    let texts: Vec<&str> = s.presets().iter().map(|e| e.text.as_str()).collect();
    // 跨 topic 按文件名序：gpt < qwen
    assert_eq!(texts, vec!["gpt-5.5", "qwen/model\n", "~/proj"]);

    // 目录不存在 → 空集，不报错（未装 deskctl 是常态）
    let mut s2 = ClipStore::new();
    s2.load_presets(&root.join("nonexistent"));
    assert!(s2.presets().is_empty());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn presets_truncate_oversized_templates() {
    let root = unique_dir("truncate");
    std::fs::create_dir_all(root.join("big")).unwrap();
    let body: String = "字".repeat(PRESET_MAX_CHARS * 2);
    std::fs::write(root.join("big/huge"), &body).unwrap();

    let mut s = ClipStore::new();
    s.load_presets(&root);
    assert_eq!(s.presets().len(), 1);
    assert_eq!(s.presets()[0].text.chars().count(), PRESET_MAX_CHARS);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn candidates_merge_history_then_presets() {
    // 预设走真实加载路径（presets 字段私有，消费者只读合并视图）
    let root = unique_dir("merge");
    std::fs::create_dir_all(root.join("snips")).unwrap();
    std::fs::write(root.join("snips/preset-a"), "preset").unwrap();
    let mut s = ClipStore::new();
    s.load_presets(&root);
    s.push("hist", 1);
    let texts: Vec<&str> = s.candidates().iter().map(|e| e.text.as_str()).collect();
    assert_eq!(texts, ["hist", "preset"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn push_truncates_oversized_clipboard_content() {
    use kime_core::clipboard::ENTRY_MAX_CHARS;
    let mut s = ClipStore::new();
    let huge: String = "a".repeat(ENTRY_MAX_CHARS * 3);
    s.push(&huge, 1);
    let cands = s.candidates();
    assert_eq!(cands.len(), 1);
    assert_eq!(cands[0].text.chars().count(), ENTRY_MAX_CHARS);
}
