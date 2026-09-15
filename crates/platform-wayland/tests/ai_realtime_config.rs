//! `ai_realtime` 配置字段测试（第六轮 WaylandShell 轨）。
//!
//! 实时候补默认必须关：上下文分词质量优先，想要 LLM 实时候补得显式打开。
//! 关键路径是**老配置文件**——第六轮之前的 config.toml 没有这个字段，
//! serde default 必须把它兜成 false，否则升级即静默开启。

use kime_core::config::Config;
use std::path::PathBuf;

/// 写一份临时配置文件。用进程号避让并行测试文件，不引 tempfile 依赖。
fn tmp_config(body: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("kime-ai-realtime-{}.toml", std::process::id()));
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
    // 老配置（无 ai_realtime 行）：serde default 兜底，升级不静默开启
    let p = tmp_config("dict_path = \"x\"\n");
    assert!(!Config::load_from_path(&p).ai_realtime);
}

#[test]
fn ai_realtime_true_parses() {
    // 显式开启的路径也得通，否则字段白加
    let p = tmp_config("dict_path = \"x\"\nai_realtime = true\n");
    assert!(Config::load_from_path(&p).ai_realtime);
}
