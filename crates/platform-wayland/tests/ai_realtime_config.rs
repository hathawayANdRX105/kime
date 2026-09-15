//! `ai_realtime` 配置字段测试（第六轮 WaylandShell 轨）。
//!
//! 实时候补默认必须关：上下文分词质量优先，想要 LLM 实时候补得显式打开。
//! 关键路径是**老配置文件**——第六轮之前的 config.toml 没有这个字段，
//! serde default 必须把它兜成 false，否则升级即静默开启。

use kime_core::config::Config;
use std::path::PathBuf;

/// 写一份临时配置文件。**文件名必须带 tag**：同一测试二进制里进程号相同，
/// 三个测试并行跑共用一个路径会互相覆盖（CI run 34952065335 实测的 race）。
fn tmp_config(tag: &str, body: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kime-ai-realtime-{tag}-{}.toml",
        std::process::id()
    ));
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn ai_realtime_defaults_false() {
    // Default impl 与 #[serde(default)] 两处都要是 false
    assert!(!Config::default().ai_realtime);
}

#[test]
fn ai_realtime_absent_from_toml_is_false() {
    let p = tmp_config("absent", "dict_path = \"x\"\n");
    assert!(!Config::load_from_path(&p).ai_realtime);
    let _ = std::fs::remove_file(&p);
}

#[test]
fn ai_realtime_true_parses() {
    let p = tmp_config("explicit", "dict_path = \"x\"\nai_realtime = true\n");
    assert!(Config::load_from_path(&p).ai_realtime);
    let _ = std::fs::remove_file(&p);
}
