//! 剪贴板候选（M16）：动态历史 + 用户预设，给平台壳提供候选来源。
//!
//! - **动态历史**：`push` 收录复制过的文本；有界（`HISTORY_CAP` 条）、去重
//!   （同文本刷新时间戳并置顶）。持久化跟随 `kime_kv` 旁表语义——不落盘，
//!   会话级（fcitx5 clipboard 插件的历史同样是会话级）。
//! - **预设**：从 deskctl snippets 目录加载（`~/.config/deskctl/snippets/
//!   <topic>/<template>`，纯文本、正文逐字节原样）。输入法不写预设，只读。
//!
//! 本模块零 I/O 依赖（预设读文件除外），历史逻辑可离线单测。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 动态历史上限：足够覆盖一天的高频复制，内存上界 ~256 × 平均长度。
pub const HISTORY_CAP: usize = 64;
/// 单条候选上限（历史与预设同规）：复制超大文本（整本书/日志）按头截断，
/// 防 IME 渲染与内存被单条拖挂。fcitx5 clipboard 插件同款封顶思路。
pub const ENTRY_MAX_CHARS: usize = 4096;
/// 兼容别名：预设截断沿用同一条上限。
pub const PRESET_MAX_CHARS: usize = ENTRY_MAX_CHARS;

/// 一条剪贴板候选：文本 + 入时间戳（毫秒）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipEntry {
    pub text: String,
    pub ts: u64,
}

impl ClipEntry {
    pub fn new(text: impl Into<String>, ts: u64) -> Self {
        Self {
            text: text.into(),
            ts,
        }
    }
}

/// 剪贴板候选集合：历史（新→旧）+ 预设（磁盘只读）。
#[derive(Debug, Default)]
pub struct ClipStore {
    history: VecDeque<ClipEntry>,
    presets: Vec<ClipEntry>,
}

impl ClipStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 收录一条复制文本。去重：已存在 → 挪到最前并刷新时间戳。
    /// 空串与纯空白不收录（复制空内容不算事件）。
    pub fn push(&mut self, text: &str, ts: u64) {
        if text.trim().is_empty() {
            return;
        }
        // 超长按头截断（ENTRY_MAX_CHARS）：防单条超大文本拖挂渲染与内存
        let text: String = text.chars().take(ENTRY_MAX_CHARS).collect();
        // 同文本已存在：移除旧位置
        self.history.retain(|e| e.text != text);
        self.history.push_front(ClipEntry::new(text, ts));
        while self.history.len() > HISTORY_CAP {
            self.history.pop_back();
        }
    }

    /// 用户预设（文件名序）。
    pub fn presets(&self) -> &[ClipEntry] {
        &self.presets
    }

    /// 合并视图：历史在前、预设在后，供候选列表直接消费。
    pub fn candidates(&self) -> Vec<&ClipEntry> {
        self.history.iter().chain(self.presets.iter()).collect()
    }

    /// 从 deskctl snippets 目录加载预设。目录不存在 → 空集（不报错，
    /// deskctl 未安装是常态）。
    pub fn load_presets(&mut self, root: &Path) {
        self.presets.clear();
        let Ok(topics) = std::fs::read_dir(root) else {
            return;
        };
        let mut files: Vec<(PathBuf, String)> = Vec::new();
        for topic in topics.flatten() {
            let Ok(templates) = std::fs::read_dir(topic.path()) else {
                continue;
            };
            for template in templates.flatten() {
                let name = template.file_name().to_string_lossy().into_owned();
                files.push((template.path(), name));
            }
        }
        files.sort_by(|a, b| a.1.cmp(&b.1));
        for (path, name) in files {
            let Ok(body) = std::fs::read_to_string(&path) else {
                continue;
            };
            let text: String = body.chars().take(PRESET_MAX_CHARS).collect();
            self.presets.push(ClipEntry::new(name, 0));
            self.presets.last_mut().unwrap().text = text;
        }
    }
}

/// 当前毫秒时间戳（壳侧调用；测试可注入任意值）。
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
