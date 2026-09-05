//! kime — 个人向拼音/双拼输入法（Rust）。
//!
//! # crate 分层（依赖只朝下）
//!
//! ```text
//! kime-pinyin        音节切分（纯逻辑，零依赖）
//! kime-shuangpin     双拼码表 → 音节（依赖 kime-pinyin 的 Reading 类型）
//! kime-core          引擎 + SQLite 持久层 + 配置（依赖上两者）
//! kime-cli           命令行入口（M1/M2 可运行验收）
//! platform-wayland   input-method-v2 壳（M3）
//! ```
//!
//! # 契约总览
//!
//! 平台壳只依赖 kime-core 的三样，core 不含任何平台代码：
//!
//! 1. [`engine::Engine`] — 有状态组合器，唯一入口 [`Engine::key`]：
//!    [`Key`] 进 → [`Outcome`] 出。`Consumed` 后壳读 [`Engine::preedit`] /
//!    [`Engine::candidates`] / [`Engine::page`] 刷 UI；`Commit` 则上屏。
//! 2. [`dict::Dict`] — SQLite 持久层（词库导入 / 查询 / 学习）。
//! 3. [`Candidate`] — 候选词统一货币：dict 产出、engine 排序、predict 追加。
//!
//! 线程模型：单线程拥有（壳事件循环内）；AI 预测走壳侧后台线程，
//! 结果经 [`Engine::merge_ai`] 回流主线程。
//!
//! 里程碑映射：segment/dict/kime-cli = M1；shuangpin/learn = M2；
//! platform-wayland = M3；merge_ai/predict = M5。详见 ROADMAP.md。
#![allow(dead_code)] // ponytail: 契约骨架期 todo!() 占位，实现时逐个移除

pub mod builder;
pub mod config;
pub mod dict;
pub mod engine;
pub mod predict;
pub mod punct;
pub mod store;
pub use dict::Candidate;
pub use engine::{Engine, Key, Outcome};
pub use kime_shuangpin::Scheme;
pub use store::FstStore;
