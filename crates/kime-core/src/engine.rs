//! 有状态组合引擎 — 平台壳消费的唯一入口。
//!
//! 契约：壳把按键换算成 [`Key`] 喂 [`Engine::key`]，按 [`Outcome`] 分派：
//! - `Consumed` → 读 preedit / candidates / page 刷 UI
//! - `Ignored`  → 放行（wayland 下 release grab）
//! - `Commit`   → commit_string(文本)，engine 内部已清空组合状态
//!
//! 内部流：letters 累积 → [`kime_pinyin::segment`]（或 kime_shuangpin 码表翻译）→
//! [`Dict::lookup`] → 候选。不做任何 I/O；切分非法时保持旧状态（无声可打即无候选）。

use crate::config::Config;
use crate::dict::{Candidate, Dict};

/// 平台无关最小按键。壳负责从 wayland / TSF / IMKit 换算进来。
#[derive(Clone, Copy, Debug)]
pub struct Key {
    /// 可打印字符；功能键为 None（翻页/选词看 code）
    pub ch: Option<char>,
    /// evdev keycode（wayland 原生即此值）
    pub code: u32,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// kime 消费了此键
    Consumed,
    /// 不归 kime（英文模式等），壳放行
    Ignored,
    /// 上屏该文本
    Commit(String),
}

pub struct Engine {
    dict: Dict,
    config: Config,
}

impl Engine {
    pub fn new(dict: Dict, config: Config) -> Self {
        Self { dict, config }
    }

    /// 中/英文模式（英文模式所有键 Ignored 直通）
    pub fn chinese(&self) -> bool {
        todo!("M1")
    }

    /// 当前 preedit：未上屏拼音串（如 "niha"）
    pub fn preedit(&self) -> &str {
        todo!("M1")
    }

    /// 当前读音的全部候选（内部 cap ~50，freq 降序）
    pub fn candidates(&self) -> &[Candidate] {
        todo!("M1")
    }

    /// (当前页, 页大小) — 候选窗布局用；M1 恒 (0, 10)
    pub fn page(&self) -> (usize, usize) {
        todo!("M4 翻页")
    }

    /// 当前高亮候选索引（候选窗渲染用）
    pub fn highlight(&self) -> usize {
        todo!("M4")
    }

    /// 唯一入口。字母累积 / 退格删音节 / 数字选词 / 空格首选 / shift 中英切换
    pub fn key(&mut self, _k: Key) -> Outcome {
        todo!("M1")
    }

    /// M5: AI 候选合入当前列表。后台线程完成后由壳回调（仍在主线程执行）
    pub fn merge_ai(&mut self, _ai: Vec<Candidate>) {
        todo!("M5")
    }
}
