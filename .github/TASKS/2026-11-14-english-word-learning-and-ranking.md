# 任务书：英文词的学习与排序（English word learning & ranking）

> 本任务书由 `jev_verify` / `jev_noul` 校准过基线事实与验收标准的可实现性。
> 全部「既有事实」条目均由 `jev_verify` 判定为 `verified`（4 条 `auto` + 1 条 `review`，
> review 那条是设计推论，已在下方标注）；验收判据的可实现性由 `jev_noul` 校准
> （概率标注在每条判据后）。

## 一、用户诉求（原始反馈）

> 「有些奇数按键还是没有对应的联想组词」
> 「那个 英文的在少量英文的时候，比如 rug、gob 这种就很奇怪，我用的少不应该放到那么前的，
> 而且我打 if、round 之类的英文单词，你是不是没对英文单词做使用次数那些分析？」

> 注：奇数键那部分**已在 PR #130 修复并合入**（`feat/surrounding-context-window`，
> merge commit `0e23b1a`）。本任务只做**英文词**这条线。

## 二、既有事实（已核实，jev_verify 全部 verified）

| # | 事实 | 位置 | jev 判定 |
|---|---|---|---|
| F1 | 英文候选的 `eff` 直接等于词库裸频，**不含任何用户提频加成**：`Candidate { freq: e.freq.max(0), eff: e.freq.max(0), .. }` | `crates/kime-core/src/dict.rs::english_to_candidate` | verified, p=0.99 |
| F2 | 中文候选的 `eff` 会加 `user_bonus_of(&user_stats, today, &e.pinyin, &e.text)` | `crates/kime-core/src/dict.rs:512` `:535` `:776` `:1281` | verified, p=0.92 |
| F3 | 纯英文文本上屏时 `segment_output_text` 返回空 Vec（`cjk_runs` 为空 → `return Ok(Vec::new())`），于是 `commit_output` **一次都不调用 `learn_commit_word`** → 英文词**不产生任何学习记录** | `crates/kime-core/src/lm.rs::segment_output_text` + `crates/kime-core/src/engine.rs::commit_output` | verified, p=0.92 |
| F4 | `user_stats: HashMap<(String, String), (u64, u64)>`，键 = `(pinyin, text)`，值 = `(使用次数 n, 最近使用日 day)`，持久化在 `kime_kv` 表 | `crates/kime-core/src/dict.rs` 字段注释 | verified, p=0.69（**设计推论**，非原文事实：英文词要进这张表需要一条独立写入路径） |
| F5 | `lookup_english` 层一（key 恰等于输入）与层二（前缀补全）**各自**按 `(freq DESC, text ASC)` 排序，层一整体在前，不跨层打平 | `crates/kime-core/src/dict.rs::lookup_english` 文档注释 | verified, p=0.90 |
| F6 | **英文模式下引擎完全不知道用户敲了什么**：英文模式时 `engine.key()` 直接 `return Outcome::Ignored`（字母键不进引擎），壳层 `route_press` 据此判定 `PressAction::Forward` 把键**转发给应用** | `crates/kime-core/src/engine.rs:479`（`if !self.chinese { … return Outcome::Ignored; }`）+ `crates/platform-wayland/src/route.rs:75` | 直接读码核实（`jev_verify` 未覆盖此条：它是链路事实而非数值断言） |

## 二之二、F6 带来的实现路径变化（**先读这段再动手**）

F6 说明：**「英文上屏时学习」这件事当前没有触发点**——
`commit_output` 只在两处被调用（Shift 冲刷切英文、标点/数字收尾等引擎内路径），
用户正常敲 `if` 的每个字母都是 `Ignored` 直通应用，**引擎全程旁观**。

因此 R1 有两条可选路径，请**读清楚现状后选一条**并在 PR 正文说明：

| 路径 | 做法 | 代价 |
|---|---|---|
| **P1 引擎侧累积** | 引擎在英文模式不再立刻 `Ignored`，改为累积本次英文串（`letters` 或独立缓冲），遇到空格/标点等**分词边界**时提交学习并 `Ignored` 放行该键 | 改引擎的英文模式状态机；要保证「累积期间不吞键」（除边界键外仍要 Forward 给应用，否则用户敲的字打不出来） |
| **P2 壳侧观测** | 壳层在 `Outcome::Ignored` 的直通路径上累积字符，遇边界键时把该串交给引擎的新接口学习 | 不改引擎按键状态机，但壳层要维护一份英文串缓冲；且 X11 前端（`platform-x11`）要走同一套，否则 XWayland 应用（微信等）学不到 |

**约束（两条路径都必须满足）**：
- 累积**不得吞键**——除作为分词边界的键外，其余键仍要转发给应用（否则用户敲的字
  打不出来，这是比学习功能更严重的回归）。
- 累积缓冲在**模式切换**（Shift 中英互切）、**焦点变化**、**Esc**、**应用收到提交**时清空，
  不得把跨句的字符攒成一个「词」。
- 边界键（空格/标点/回车）本身**要转发给应用**（它负责真正上屏），
  学习只是**旁路观察**，不能改变按键的可见行为。

> 实现前请先读 `crates/platform-wayland/src/route.rs` 的 `route_press` /
> `PressAction::Forward` 语义，与 `crates/platform-x11/src/xim.rs` 的按键转发路径，
> 确认你的路径对 **Wayland 与 XIM 两个前端**都成立（或明确说明只覆盖 Wayland 及原因）。

## 三、根因（由 F1+F3 推出，jev_noul 判定 p=0.97 likely）

要让英文词的使用次数真正影响排序，**必须同时实现两件事**，只做一件无效：

| 缺口 | 现状 | 需要 |
|---|---|---|
| G1 **没有学习** | 英文上屏不写 `user_stats`（F3） | 英文上屏时按词切分后写入学习记录 |
| G2 **没有提频** | 英文候选 `eff` 无用户加成（F1） | 英文候选查询时按 `user_bonus_of` 计算 `eff` |

**只做 G2 无效**：若英文词从未进 `user_stats`，`user_bonus_of` 对它恒返回 0（jev_noul p=0.92 likely）。
**只做 G1 无效**：排序不读 `user_stats`，加成不影响名次（jev_noul p=0.36 → uncertain，
即「只排序不学习」**不**足以解决问题）。

## 四、功能要求

### R1 英文上屏要能学习（对应 G1）

英文模式下用户敲的词上屏时，产生学习记录，使用次数计入 `user_stats`。

**必须先切词再学**：把纯英文文本**整串**当一个词写进用户词表，会让用户误打的任意
英文串都进表，污染候选（jev_noul p=0.98 likely）。因此需要：

- 英文串按**空格/标点**切成词（或用词库 `english` 表的分词能力，若已有）；
- 只有**在 `english` 表里存在**的词才学（词典校验，避免乱串入表）；
- 校验失败/查不到的串**不学**。

> **需实现者确认的设计点**（jev_noul p=0.84 uncertain，非硬性结论）：
> 写入 `user_stats` 时 `pinyin` 字段用**英文词本身**（与 `english_to_candidate` 里
> `pinyin: e.text.clone()` 的既有约定一致），`text` 字段用**上屏时的原始大小写形态**
> （`AA` 与 `aa` 是两条独立条目，`lookup_english` 大小写不敏感匹配）。
> 实现前请读 `dict.rs` 的 `user_stats` 键约定与 `learn_mem` 的写入路径再定，
> 并在 PR 正文写明你的选择与理由。

### R2 英文候选排序要含用户提频（对应 G2）

`lookup_english`（或 `english_to_candidate`）产出的候选，`eff` 要按与中文路径**同口径**
计算：`eff = freq.max(0) + user_bonus_of(&user_stats, today, <pinyin>, <text>)`。

- 同口径 = 同一个 `user_bonus_of` 函数、同一套 `(n, day)` 语义（含 30 天半衰期、
  首用即满额等既有策略，见 `dict.rs:57` 的函数注释）。
- 排序仍须满足 F5 的分层契约：**层一整块在层二之前**，层内按
  `(eff DESC, text ASC)`。不要为了提频把层二词插到层一之前。

### R3 不破坏既有英文候选行为

- 层一/层二的分层、单字母闸（`letters.len() < 2` 不查）、`candidate_limit` 截断、
  大小写不敏感匹配、`AA`/`aa` 独立条目——全部保持。
- 中文候选的排序与学习行为**零改动**。

## 五、验收判据（可判定；每条附 jev 校准）

> 判据要写成**能直接转成测试断言**的形式。括号里的概率是 jev_noul/jev_verify 的
> 校准结果，标注「uncertain」的条目是**设计选择**，实现者可调整，但必须在 PR 正文
> 说明选择理由。

**A1**（F1/F2 同口径，p=0.99 verified）
`lookup_english` 产出的候选 `eff` 等于 `freq.max(0) + user_bonus_of(...)`，
与 `dict.rs:512` 中文路径的算法**逐字同口径**。

**A2**（G1 生效，p=0.97 likely）
同一英文词上屏 N 次后，`user_stats` 中该键的计数 n 增加 N；
查询该词时其 `eff` 相比未使用时**上升**（加成 > 0）。

**A3**（用户原始诉求的直接验收）
在 `english` 表存在 `if`/`round`（高频）与 `rug`/`gob`（低频）的前提下，
用户上屏 `if` 若干次后，查询 `if` 时它排在 `rug`/`gob` **之前**；
未上屏过 `rug`/`gob` 时它们不应靠前。

**A4**（防污染，p=0.98 likely）
上屏一个**不在 `english` 表里**的英文串（例如 `zxqv`）后，
`user_stats` **不新增**该串的记录，且 `lookup_english` 的候选集合不变。

**A5**（F5 分层契约）
有用户提频的层二词（如 `ifx`）**不得**插到层一词（如 `if`）之前。

**A6**（不回归）
中文候选排序、中文学习、英文候选的分层与截断行为与改动前一致
（既有测试全绿 + 新增测试覆盖 A1..A5）。

**A7**（不吞键——F6 路径选择的硬约束，可直接写成测试）
累积英文串期间，除作为分词边界的键外，**每个字母键仍须转发给应用**：
在 `crates/platform-wayland/tests/key_routing.rs` 已有 `route_press` 纯函数测试的
风格下，补一条断言——英文模式下按字母键得到的 `PressAction` 仍是
`Forward`（不是 `Consume`），且**首字母与后续字母都一样**。
若走 P2（壳侧观测）路径，这条断言的落点是壳层新逻辑；
若走 P1（引擎侧累积）路径，落点是 `route_press` 对引擎新 `Outcome` 的映射——
**无论走哪条，字母键的可见行为都必须与改动前逐字一致**。

> 参考：`route.rs:75` 的 `Outcome::Ignored => PressAction::Forward` 是改动前的既有
> 行为；改动后英文模式下按字母键必须仍是这个结果。

## 六、实现提示（省你时间）

- `crates/kime-core/src/dict.rs` 是主战场：
  - `english_to_candidate`（当前 `eff = freq`）→ 加成计算点；
  - `lookup_english`（当前层内按 `freq DESC` 排）→ 改成按 `eff DESC` 排；
  - `user_stats` / `learn_mem` / `enqueue_learn` / `persist_learn` → 英文学习要复用的写入路径。
- `crates/kime-core/src/engine.rs::commit_output`（约 838 行）是学习的触发点：
  它按 `segment_output_text` 的结果逐词 `learn_commit_word`；英文需要另一条分支。
- 英文模式的上屏入口：`engine.rs:474-475`（Shift 冲刷切英文时 `commit_output(text)`）
  与英文模式下按键直通路径（`engine.rs:479` 附近 `if !self.chinese { return Outcome::Ignored }`，
  注意英文模式的字母键是**壳层直通**还是走引擎——**先读清楚**再决定学习触发点）。
- `dict.rs:57` 的 `user_bonus_of` 有完整语义注释（30 天半衰期、首用即满额），
  复用它，不要另写一份加成算法。

## 七、仓库约定（务必遵守）

- **主仓 `/home/hathaway/projects/kime` 只读**；代码改动必须在独立 worktree：
  `git worktree add .wt/<name> -b <branch>`（本任务分支已建好，见下方「分支」）。
- **本地禁止任何 `cargo build` / `cargo test` / `cargo run` / `cargo check`**
  （含单个测试、example、`--bin`）：编译与测试一律放 PR 的 CI 跑
  （`.github/workflows/ci.yml`：fmt + clippy + test + build + benchmark + rust-specs dylint）。
  需要复现行为时**写成测试文件提交进仓库**，由 CI 跑，不在本地执行。
- 本地允许：读代码、grep/glob、`git` 操作、写文件、跑 `rustfmt`（单文件格式化允许）。
- 不要动 `.githooks/`（canon 领地）、不要动用户的未跟踪目录 `checks/`。
- 提交身份固定 `hathawayANdRX105`；**禁止** `git -c user.name=...` 覆盖身份。
- commit 走 conventional commit；**commit 类型必须与 PR 标题类型一致**（canon CM-03 门禁）。
- 合并只走 squash：`gh pr merge <N> --squash --delete-branch`。
- 新增的 dylint 抑制必须写成
  `#[cfg_attr(dylint_lib = "<库名>", allow(<lint 名>))]`
  （裸 `#[allow(...)]` 在普通 rustc/clippy 下是 `unknown lint`，`-D warnings` 直接失败）；
  属性要排在文档注释**之前**（门禁 `ts_extract` 取「紧邻 item 的连续 `///` 行」）。

## 八、分支与 PR

- 分支：`feat/english-word-learning-and-ranking`（**已创建**，基于 `main` 的 `0e23b1a`）
- 建议 PR 标题：`feat: learn english word usage and rank candidates by user frequency`
- PR 正文用仓库模板 `.github/PULL_REQUEST_TEMPLATE.md`，其中：
  - `## What` 写 R1/R2/R3 三条需求
  - `## Why` 引本任务书第三节的根因（**必须同时做两件事**）
  - `## Construction plan` 拆成可勾选的实现步骤
  - `## Delivery record` 逐条登记 A1..A6 的验证方式与结果
  - `## Review record` 登记发现与处置（零未跟踪项）

## 九、明确不在本任务范围

- 奇数键末键联想（已在 #130 修完并合入）。
- surrounding_text 上下文窗口（已在 #130 从 32 放大到 320）。
- 英文**补全**（层二）的语义改动——本任务只动排序口径与学习，不改「哪些词被查到」。
- XWayland / 终端应用拿不到 surrounding_text 的边界（既有 README 条目，优雅退化）。
