<!-- managed by canon agents.yaml @ 2026-09-24 -->
## kime 约定

### 开工前

1. 读本文件（`AGENTS.md`）。
2. 读 `README.md` 与 `ROADMAP.md`，确认当前里程碑顺序。
3. 检查当前分支与未提交改动；禁止覆盖未提交代码。

### 开发方式

- `.wt/<name>/` 是开发工作目录：每个子任务用 `git worktree add .wt/<name> -b <branch>` 挂独立分支；主仓库根目录只读（除根 `Cargo.toml` 变更）。
- **本地禁止任何 `cargo build` / `cargo test` / `cargo run`**（含单个测试、example、`--bin`）：编译与测试一律放 PR 的 CI（`.github/workflows/ci.yml`：fmt + clippy + test）。本地不验证正确性，靠 CI 绿灯为准；需要复现行为时写成**测试文件或 example 提交进仓库**，由 CI 跑，不在本地执行。
- 本地允许的仅：读代码、grep/glob、`git` 操作、写文件；不产生任何 target/ 产物。
- bench（`cargo bench`）不跑 CI，需要时本地跑且必须套 `cpulimit -l 65 -i --`。

### `.wt/` 工作目录保护（硬约束）

`.wt/<name>/` 是各子代理的工作目录。**严禁在未经确认的情况下删除整个 `.wt/` 目录或批量 `rm -rf .wt/*`。**
- 只能删除自己负责的完成子任务的 worktree：`git worktree remove .wt/<name> --force`。
- 子代理禁止操作 `.git` 内部文件、禁止执行 `git clean -fd` 或随意 `git init`。

### 目录分层与职责

```text
bin/kime/             CLI 二进制入口（REPL、build-dict 命令）
crates/kime-core/     引擎核心：状态机、SQLite、FST/内存索引、配置
crates/kime-pinyin/   纯拼音音节切分（404 音节，零外部依赖）
crates/kime-shuangpin/双拼码表（小鹤/自然码查表，依赖 kime-pinyin 的 Reading）
crates/platform-wayland/ input-method-v2 客户端 + 候选窗（layer-shell / popup）
benches/              基准测试套件（cargo bench --bench kime_bench）
```

- 依赖单向向下：`bin/kime -> platform-wayland -> kime-core -> kime-shuangpin -> kime-pinyin`。
- core 不碰任何显示/UI；shell 只做「按键进、候选出、上屏提交」。

### 安全与环境约定

#### 文件删除

- 一律使用 `gio trash <path>`（可恢复），禁止使用 `rm` / `rm -rf`。
- VCS 跟踪的文件可以使用 `git rm`。

#### cpulimit（硬约束）

- 编译、测试、装包、基准测试一律加限制：`cpulimit -l 65 -i -- cargo <cmd>`。
- git、grep、文件读写等轻量命令不需要。

#### 键盘/输入法真机会话约定

- 用户桌面环境为 **mangowm**（Wayland，`WAYLAND_DISPLAY=wayland-0`）。
- 测试前**必须停用 fcitx5**：`systemctl --user stop 'dbus-:1.2-org.fcitx.Fcitx5@1.service'` 并 `pkill fcitx5`。
- 测试结束**必须恢复 fcitx5**：`setsid fcitx5 -d >/dev/null 2>&1`。
- **严禁全屏测试窗口**（如 `foot -F` 会导致黑屏遮罩）。
- **已知边界**：微信等 XWayland 应用不走 text-input 协议，光标位置不跟随时不要误报为代码 bug。

### 主控 / 子代理编排规范

你是主控 agent：编排任务、派子代理执行、审查子代理产出，**不要亲自把核心实现写完**。

1. **工作目录门禁**：子代理必须在 `.wt/<branch>` 工作，prompt 必须写明绝对路径 cwd。
2. **任务量门禁**：单个子任务 ≤ 5 个文件、单一主题、单一修改范围。
3. **真实复验（Audit）**：不轻信子代理自报的 "ALL PASS"——必须检查 diff 边界、核对真实测试计数；**验收命令一律由 PR 的 CI 跑**（见「开发方式」），主控本地不执行 cargo，靠 CI 绿灯 + 产物判断。
4. **工具审查**：修改核心逻辑后先跑 `code-review-graph detect-changes` 检查结构面风险。
5. **收尾报备**：汇报改了哪些文件、跑了哪些测试、性能对比、剩余风险。

## 发现处置纪律

自动检查（gate 的 `FAIL`/`WARN`、`jev` L3 语义发现、CRG / `ocr review` 审查意见）
产出的是**发现**，不是判决。每条发现都必须被显式处置，不存在"绕过"这个选项。

### 先读规范，再改代码

1. 拿到 finding，先读规则原文，确认这条发现到底要求什么：
   - gate 规则总览：`.githooks/GATE_HANDBOOK.md`（无则 `canon/manual/gate.md`）
   - 单条规则的参数（匹配范围 / 严重度 / harness）：`.githooks/spec/**/<rule>.yaml`
   - 项目适配说明（本仓为什么这么定）：`.agent/rules/gates.md`
2. 不确定 finding 是否成立时，读完规则仍不能判定 → **记为待裁决**并在交付记录里写明，
   不要凭猜测改代码，也不要直接忽略。

### 按根因修，不按症状修

- finding 指向的**约束**是根因。修代码使约束成立，而不是让检查不再报。
- 修完自问：这条约束在本仓还成立吗？下次同类改动还会不会触发？

### 完整读输出，不截断

- 拦截信息**逐条读完**再动手。`| head -5`、`| tail`、`grep -v` 会吞掉后面的 finding，
  让人误以为已经修完。
- 报告里出现「N checks passed」时，确认 N 覆盖了你改动的部分。

### 禁止糊弄式修复

以下动作一律视为违规（无论 gate 是否因此变绿）：

| 禁止 | 为什么 | 正确做法 |
|---|---|---|
| 改 `.githooks/spec/` 规则、降低 `fail_severity`、删 spec 文件 | 把约束改没，不是修问题 | 开 issue 说明规则缺陷，交维护者决定 |
| `--no-verify`、跳过钩子、直接推 | 绕过的是整个门禁体系 | 修到清零；规则有误走 issue |
| `head` / `tail` / `grep -v` 截断输出后当没看见 | 后面的 finding 被吞 | 完整读输出 |
| 加 `#[allow(dead_code)]` / `# noqa` 消告警 | 压制信号而非解决 | 删无用代码，或写清保留理由 |
| 建空文件 / 空目录 / 占位文件骗过目录类规则 | 结构噪音 | 真按规则合并或删除 |
| 给无断言测试塞 `assert!(true)` | 测试变成永真装饰 | 断言真实行为；无行为可测就删测试 |
| 拆分 / 改名 / 移动只为躲过匹配范围 | 破坏结构换绿灯 | 按规则设计的结构改 |

### 逐条处置并留下书面说明

- **每条 finding 一个处置**：修复（默认）或**书面驳回**。
- 修复 → 在交付记录里写：`规则 ID → 根因 → 改法（file:line）`。
- 驳回 → 必须写 `规则 ID + 不修理由 + 依据`，由维护者裁决。沉默即违规。
- 交付记录落点：PR 正文 `## Delivery record` 段，或 issue 的交付评论。
- WARN 与 FAIL 同等对待。WARN 只是不拦，不是可忽略。

### 规范层级

- `.githooks/` 是 gate 领地：agent 不改规则。
- `.agent/rules/`、`specs/rules/` 是规范正本：发现规则与现实冲突 → 提 issue，不自行改写。
- 本纪律与各仓既有条款冲突时，以本纪律为准（它更严格）。

## 代码风格

### 命名与结构

- 函数名动宾结构、见名知目的（`parse_channel_config` 而不是 `do_config`）。
- 公共 API 写文档注释（用途、参数、错误、示例），模块头写 `//!`。
- 变量与类型不缩写到看不出含义；短名只留给公认短物（`id`、`ctx`、`err`）。

### 注释

- 注释写**为什么**，不复述代码在做什么。
- 不留 AI 味注释（`// Step 1:` / `// This function` / `// 该函数…` / `// 首先…然后…`）。
- 需要解释的复杂逻辑，宁可提取成命名清晰的函数，也不要靠注释块描述流程。
- 注释掉的代码直接删；git 记得它。

### 占位符与未完成

- 未实现的函数或 trait 用语言原生宏，并带 issue 号：
  - Rust：`todo!("TODO(#123): 说明这里要做什么")` / `unimplemented!("…")`
- TODO / FIXME 注释必须带 issue 号：`// TODO(#123): …`。
- 不留空的 `todo!()` / `pass` / `NotImplemented` 桩而无说明。

### 复用与删除

- 动手前先找同仓同类实现与已装依赖。已有工具能解决就不新写。
- 新增依赖前确认：标准库能做完？已装依赖能做？确实都需要才加。
- **删除优于新增**：不留兼容垫片、旧别名、废弃分支、注释掉的旧实现。
- 改了接口就同步迁移所有调用方，不留双路径兼容。

### 工具

- 命名、缩进、格式化交给项目工具（`cargo fmt` / `gofmt` / `ruff format` / `prettier` / `biome`），
  不手工对齐，不在格式化工具之外争论风格。
- lint 报错逐条判断：真问题就修；误报就在规则允许的方式下局部豁免并写明理由，
  不整文件关掉。

## 构建与验证

### 基线

- 改动前先确认基线状态。基线已经红就先说清，别把自己的问题和既有问题混在一起报。

### 验证行为，不是验证代码存在

- 改完跑**真实命令**验证："跑一下" = 启动实际程序、调用实际接口、发真实请求、观察输出或状态。
- bug 修复先复现再修，修完确认复现路径不再触发。
- 永久性改动要留一个能抓住真实回归的检查。
- 测可观察行为与边界：状态迁移、转换、优先级、真实错误、边界值。
  不测 plumbing、不断言源码文本、不写永真断言、不测 mock 的回声。
- 测试与被测文件就近放 `tests/`（同名对应），保持全量套件可通过。

### 重命令放对位置

- 全量测试、全量构建、全量 lint 放 CI 或收尾阶段，不在改动过程中反复跑。
- 本地只跑轻量、快的针对性检查（单 crate `cargo check`、单包测试、`fmt --check`、
  类型检查）。
- 需要本地跑重命令时，套资源限制（`cpulimit -l 65 -i --` 或本仓等价手段），
  不抢占用户正在用的 CPU。
- 装依赖、打包等命令同样受限。

### 收尾

- 一次跑完该跑的检查（测试 + lint + 类型），不在半成品状态下宣称通过。
- 验证不了的部分（缺运行环境、缺凭据、缺硬件）明确说"未验证 + 为什么"，
  不把"没跑"说成"通过"。
- 不因为失败就改测试迎合实现。测试红了先判断是实现错还是测试错。

## 破坏性操作与敏感信息

### 删除

- 删文件前确认它确实是废弃物（生成物、已合并的临时文件），不是"看起来没用"。
- 用可恢复的方式删（`gio trash`），不用不可恢复的直接删除。
- `rm -rf`、覆盖写、清空数据库这类不可逆操作：**先说明影响，等确认**。
- 删的是别人的产物、你不理解用途的文件、或 gitignore 里的东西 → 停下来问。

### 敏感与不可逆

- 凭据、token、密钥、私钥：不打印到输出、不写进提交、不粘到 issue/PR 正文。
- 不擅自 dump 整个配置文件或环境变量（可能含密钥）。要看就只看需要的字段。
- 系统级配置、字体、全局环境、dotfiles 里的全局项：默认别动，改动前先问。
- 数据库迁移、配置格式变更、依赖大版本升级：先确认可回滚。

### 安装与全局改动

- 装包、改 PATH、装 systemd 服务、改 shell 配置：先确认再动。
- 写进 dotbot / 配置管理器托管范围的路径前，先确认该由谁管。
- 不可逆的系统级改动（分区、引导、网络栈）一律先问，不自行执行。

## 提交与 PR

### 分支

- 默认分支是 `main`（本仓若不同以本仓为准），功能从默认分支拉。
- 一个任务一个分支，分支名带类型前缀（`feat/` / `fix/` / `refactor/` / `chore/`）。
- 合并后清理已合并分支与 worktree，不留 stale 分支。

### Commit

- 标题走 conventional commit（`feat:` / `fix:` / `refactor:` / `docs:` / `chore:` /
  `test:` / `ci:` / `build:` / `perf:` / `style:` / `revert:`）。
- 标题**用英文**，正文可用中文。
- 一个 commit 一件事。不把无关改动、格式化噪声、生成物混进逻辑改动。
- 提交前跑对应检查（`gate pre-commit` / `gate pre-push`），不靠推送失败才发现。

### Issue

- 标题中文；正文 heading 英文、内容中文。
- sub-issue 必须自包含：正文不写 `Parent:` / `Related:` / PR 占位符，直接写清它要什么。
- 关闭前 `Done when` 的 checkbox 全勾。

### PR

- 标题纯英文（conventional commit 风格）；正文小节标题英文、内容中文。
- 正文按仓库模板（`.github/PULL_REQUEST_TEMPLATE.md`）写：背景 / 改了什么 / 为什么 /
  实现步骤 / 交付记录 / 怎么验证 / 检查清单。
- 关联 issue 用 `Fixes #<n>` 收尾行；draft 阶段用 `Related #<n>`，合并授权前改 `Fixes`。
- 开启或更新 PR 后看 CI 结果到底（`gh pr checks`），红了就修，不等用户来问。
- 被 gate 拦下就修代码，**不改规则**。规则确有缺陷 → 开 issue 交维护者裁决。

### 收尾

- 收尾时清掉：已合并分支、临时 worktree、临时进程、跑完的 dev server。
- 资源及时释放；只保留维护者需要的进程（如用户要看的 web 前端）。
