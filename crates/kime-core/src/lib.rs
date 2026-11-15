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
//!
//! # 候选排序架构（重构后的设计要点）
//!
//! 这一节是候选生成与排序的**设计正本**。改排序前先读这里，再读
//! [`model`] 与 [`rank`] 的模块文档；三处说法若不一致，以代码与测试为准并回来修文档。
//!
//! ## 1. 候选自携语义，顺序不再靠位置推断
//!
//! 旧实现里「这条候选是精确层还是补全层」「它是中文还是英文」都编码在**位置**
//! （先 push 的在前）与**读音字段的内容**（英文候选把词本身塞进读音字段）里。任何
//! 一次 `insert`/`extend`/`sort` 都可能静默改层，英文分层判断则寄生在
//! 「读音字段 == 按键串」这条隐含约定上。
//!
//! 现在每条候选自携 [`model::Lang`] / [`model::Layer`] / [`model::SourceKind`]
//! （见 [`model::Candidate`]）：层与语言是**数据**，在产生阶段定型，之后只读。
//!
//! ## 2. 一个比较器，一把尺
//!
//! 全部候选顺序由 [`rank::Ranker::compare`] 决定：
//!
//! ```text
//! 同语言： (layer, score DESC, text ASC, source)
//! 跨语言： (score DESC, text ASC, source)          ← 只比分
//! ```
//!
//! ## 3. 跨语言比的是**校准分数**，不是原始频率
//!
//! 中文词条频率 10⁵~10⁷、英文词频 10³ 量级，直接比原始数值等于「中文恒赢」，
//! 那只是把语言分块换个说法。所以每个语言在自己的分布里对数归一化：
//!
//! ```text
//! score = ln(1 + eff) × SCORE_MAX / ln(1 + max_eff_of_that_language)
//! ```
//!
//! 于是「中文『如果』压过英文 `rug`」是**分数**的结论（真实词库上 `ru` 的候选里
//! 如果 在第 3 位、`rug` 在第 27 位），而不是「中文被强制排在前」；高频英文词同样
//! 能压过低频中文词。
//!
//! 单调性优先于饱和：分数**不截断到上界**——用户提频超过词表最大频时（BOOST 叠加后
//! 常见），截断会让超额词并列满分，排序退化成文本序，提频静默失效。
//!
//! ## 4. 层只在同语言内生效
//!
//! 「精确命中整体在补全之前」是**词典单次查询内部**的契约，因此只在 `lang` 相同时
//! 比 layer。跨语言时 layer 不参与——否则「英文精确词」又会被「中文补全词」按层号
//! 压住，等于把语言分块换个位置重演一遍。
//!
//! 例外只有一个：[`model::Layer::Append`]（首音节单字、AI 候选）不参与整表排序，
//! 它们按分数并入主列表，是「层完整性」唯一被允许的破口。
//!
//! ## 5. 两条解码路径的合批只看分数
//!
//! 双拼末键半截时会同时查「声母位 + 半截键」与「韵母位完整音节」两种意图。这两批
//! 候选的层号之间没有可比性（一次查询的精确块 vs 另一次查询的精确块），所以合批用
//! [`rank::Ranker::sort_by_score`]：只按分数重排，不按层。
//!
//! 历史上同时存在三份不等价比较器（词典条目序、候选序、engine 内联序），任何调参都要
//! 三处同步；现在只有这一份。
//! ## 6. 原始频率与有效频率分离
//!
//! [`Candidate::freq`] 恒为库内原始值（导出与持久化契约）；
//! [`Candidate::eff`] 是排序用有效频率 = 裸频 + 用户学习加成（开库/学习时烘焙）。
//! 加成永不写回 `freq`，否则「这个词原本多少频」这一事实会被不可逆地覆盖。
//!
//! ## 7. 两类加成，两种生命周期
//!
//! - **用户学习加成**：持久化、跨会话，烘焙进 `eff`（`dict::user_bonus_of`）。
//! - **LM 上下文加成**：只在本次查询有效，不写 `eff`（`eff` 必须保持
//!   「裸频 + 用户加成」这一可断言的量），由 [`rank::CtxBoost`] 在比较时叠加。
//!
//! 比较键用 `u64` 饱和加。旧实现是 `eff as i64 + lm_boost`：超过 `i64::MAX` 时
//! `as` 截断成负数，会静默反转高频段顺序。
//!
//! ## 8. 跨批去重归排序内核
//!
//! 多路查询共享一份 [`rank::Seen`]，追加走
//! [`rank::append_unique`]（先到先得），需要「同文本保留更高频但位置留在首次
//! 出现处」时用 [`rank::merge_unique`] 的 [`rank::Dedup`] 策略。engine 里不再有
//! 手写的 `HashSet` + `retain` 去重循环——去重口径只有一处定义，策略必须显式
//! 选择，避免同一份候选在不同批次里按不同规则保留。
//!
//! ## 9. 唯一的跨层归并：首音节单字
//!
//! [`model::Layer::Append`] 标记「不参与整表排序、由各自的追加策略处理」的候选
//! （首音节单字、AI 预测）。首音节单字按分数与主列表双列归并（高频首字要能越过
//! 弱补全），这是唯一一处分数可以跨越层边界的地方，因此单独建层而不是给它
//! 一个普通层号——否则「精确层不可被跨层超越」会出现一个没人记得的例外。
//!
//! ## 10. 双存储路径共用一套语义
//!
//! 内存有序索引与只读块存储（`store`）都只负责「取词 + 裸频」；层归属、用户词
//! overlay 合并、排序全部在 [`dict`] 层完成。前缀上界只有一份实现
//! （[`model::prefix_upper_bound`]），避免两条路径切出不同的补全区间。
//!
//! ## 11. 仍然存在的已知耦合（有意保留，非疏漏）
//!
//! - `Engine::refresh_candidates` 里双拼与全拼仍是两条分支：两者在「半截键 /
//!   非法键对 / pending 尾巴」上的状态机不同（`sp_active`、`preedit`、键位消耗），
//!   合并成一条管道需要先抽出统一的解码结果类型；当前重复的是**编排**而非排序语义。
//! - 用户学习虽已统一到 [`learn`] 的键与算式（`LearnKey` 带语言标签、`UserStats`
//!   是唯一计数容器），但**写入时机**仍分两处：`learn_mem`（中文，写 phrase +
//!   旁表）与 `learn_english`（英文，只写旁表）。差异来自 phrase 表只收中文词条，
//!   属于存储契约而非排序语义。
//! - 存储层（`dict` 的内存索引 / `store` 的只读块）尚未抽成 trait：两条路径的
//!   「取词」逻辑形状不同（二分区间 vs 块解码 + topk 热表），强行统一会引入
//!   一层只在编译期有用的间接。前缀上界与层归属已收敛到一处（见第 7 节）。
#![allow(dead_code)] // ponytail: 契约骨架期 todo!() 占位，实现时逐个移除

pub mod builder;
pub mod clipboard;
pub mod config;
pub mod context;
pub mod correction;
pub mod dict;
pub mod engine;
pub mod lattice;
pub mod learn;
mod learn_writer;
pub mod llm;
pub mod lm;
pub mod model;
pub mod punct;
pub mod rank;
pub mod store;
pub mod typing;
pub use clipboard::ClipStore;
pub use context::ContextTail;
pub use engine::{Engine, Key, Outcome};
pub use kime_shuangpin::Scheme;
pub use model::{Candidate, Lang, Layer, SourceKind};
pub use store::FstStore;
