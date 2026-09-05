# kime 开发约定（基于 Ferrite 规范适配）

## 开工前

1. 读本文件（`AGENTS.md`）。
2. 读 `README.md` 与 `ROADMAP.md`，确认当前里程碑顺序。
3. 检查当前分支与未提交改动；禁止覆盖未提交代码。

## 开发方式

- `.wt/<name>/` 是开发工作目录：每个子任务用 `git worktree add .wt/<name> -b <branch>` 挂独立分支；主仓库根目录只读（除根 `Cargo.toml` 变更）。
- 本地只跑 `cargo check`；CPU-heavy 命令必须套 `cpulimit -l 60 -i --`。
- 本地有 192 万词条的 SQLite 库（`~/.local/share/kime/dict.sqlite3`），跑基准时优先复用。

## `.wt/` 工作目录保护（硬约束）

`.wt/<name>/` 是各子代理的工作目录。**严禁在未经确认的情况下删除整个 `.wt/` 目录或批量 `rm -rf .wt/*`。**
- 只能删除自己负责的完成子任务的 worktree：`git worktree remove .wt/<name> --force`。
- 子代理禁止操作 `.git` 内部文件、禁止执行 `git clean -fd` 或随意 `git init`。

## 目录分层与职责

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

## 安全与环境约定

### 文件删除

- 一律使用 `gio trash <path>`（可恢复），禁止使用 `rm` / `rm -rf`。
- VCS 跟踪的文件可以使用 `git rm`。

### cpulimit（硬约束）

- 编译、测试、装包、基准测试一律加限制：`cpulimit -l 60 -i -- cargo <cmd>`。
- git、grep、文件读写等轻量命令不需要。

### 键盘/输入法真机会话约定

- 用户桌面环境为 **mangowm**（Wayland，`WAYLAND_DISPLAY=wayland-0`）。
- 测试前**必须停用 fcitx5**：`systemctl --user stop 'dbus-:1.2-org.fcitx.Fcitx5@1.service'` 并 `pkill fcitx5`。
- 测试结束**必须恢复 fcitx5**：`setsid fcitx5 -d >/dev/null 2>&1`。
- **严禁全屏测试窗口**（如 `foot -F` 会导致黑屏遮罩）。
- **已知边界**：微信等 XWayland 应用不走 text-input 协议，光标位置不跟随时不要误报为代码 bug。

## 主控 / 子代理编排规范

你是主控 agent：编排任务、派子代理执行、审查子代理产出，**不要亲自把核心实现写完**。

1. **工作目录门禁**：子代理必须在 `.wt/<branch>` 工作，prompt 必须写明绝对路径 cwd。
2. **任务量门禁**：单个子任务 ≤ 5 个文件、单一主题、单一修改范围。
3. **真实复验（Audit）**：不轻信子代理自报的 "ALL PASS"——必须真跑验收命令、检查 diff 边界、核对真实测试计数。
4. **工具审查**：修改核心逻辑后先跑 `code-review-graph detect-changes` 检查结构面风险。
5. **收尾报备**：汇报改了哪些文件、跑了哪些测试、性能对比、剩余风险。
