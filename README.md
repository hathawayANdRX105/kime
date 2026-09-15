# kime

个人向 Rust 拼音/双拼输入法。Linux Wayland 优先（mangowm 实机验证），Windows/macOS 靠平台壳后补。

## 架构

```
bin/kime/             CLI 入口（REPL、词库编译）
crates/kime-core/     引擎核心：状态机、持久化、FST/内存索引、上下文通道
crates/kime-pinyin/   纯拼音音节切分（404 音节表，零依赖）
crates/kime-shuangpin/双拼码表（小鹤/自然码，纯查表）
crates/platform-wayland/ input-method-v2 客户端 + input-popup 候选窗（进程内渲染）
platform-win/         （规划中）Windows TSF 壳
platform-mac/         （规划中）macOS IMKit 壳
```

## 功能

### 组句与排序
- **Word Lattice + Viterbi**：多切分查询（蛋糕 bug）、k-best 整句联想、概率代价模型（计数比会碎切占便宜，实测「知道」输给「知+道」的病态已钉回归）
- **用户调频（方案 B）**：选词即提升 + 30 天半衰期衰减，`kime_kv` 旁表计数；加成只进比较器，导出频率恒为库内原值
- **4 音节混排**：`PATHS_PER_NODE=3`，2+2 组合与整句同列按频率排
- **邻键纠错（第七轮）**：直查候选不足时按编辑距离 1（QWERTY 邻键替换 + 相邻转位）重查，`xain`→「先/现/线」、`zhant`→zhang 词；纠错候选永远排在精确结果之后，`correction = false` 可关
- **双拼半截键补全**：零声母半截键（y/w/元音，公共前缀塌缩为空）枚举完整音节、末音节匹配的词优先——ziranma `ke`+`y` → 「可以」类 ke'yi 词置顶，单键 `y` → yi 系词在前；有公共前缀的键（u→sh）行为不变

### 上下文感知（第六轮）
- **surrounding_text 捕获**：input-method-v2 四事件（surrounding_text / text_change_cause / content_type / done）双缓冲批处理，done 才提交
- **回声过滤**：`cause=INPUT_METHOD`（自己上屏）不推引擎，防自激循环
- **种子先验**：上文末词经 `readings_of_text` 反查读音，作 lattice 虚拟起点参与联合概率（不进产出文本）
- **边界**：XWayland 应用（微信/QQ）与无 text-input 的应用拿不到上下文 → 优雅退化为纯拼音行为

### AI 候选
- OpenAI 兼容端点（`ai_endpoint`），LLM 请求带上文
- **实时候补默认关闭**（`ai_realtime = false`）：候选质量优先；需要时在 config.toml 显式打开

### 编辑与按键
- 组合内光标 C-f / C-b / C-h（keysym 兜底，xkb Ctrl 变换不吞键）
- 壳内按键自动重复（Backspace / C-f / C-b / C-h 长按连续）
- Shift 手势：点击切换中英，按住期间临时英文透传不改模式（rime ascii_composer 语义）
- **已知边界**（详见 `todo/HANDOFF.md`）：`place_sentences`/engine 去重合并与 top_user 列表仍按裸 freq 比较（boost 不跨列、不入个性化列表）；常驻进程跨天不刷新 `today` 缓存 → 当晚 boost 少衰减 ≤1 天；升级前老用户词无使用计数（n 从 1 重计）

## 性能指标（192 万词条实测）

| 指标 | SQLite 基线 (M1-M6) | 内存排序索引 (M6.5) | FST 二进制词库 (M7) |
|---|---|---|---|
| **词库内存** | 185MB (磁盘) | **228.8MB** (堆) | **41.7MB** (mmap，按需驻留) |
| **词库载入** | 0.05s | 2.86s | **0.55s** |
| `ni'hao` 前缀查询 | 41.66ms | 0.007ms | **0.015ms** |
| `shen'me` 前缀查询 | 32.54ms | 0.305ms | **0.438ms** |
| `nh` 声母缩写查询 | ~20ms | 0.089ms | **0.0002ms** |

## 快速开始

### 1. 构建 FST 词库（一次性，将 192 万词条编译为 42MB 二进制）
```bash
cargo run --release -p kime -- build-dict \
  --in ~/.local/share/kime/dict.sqlite3 \
  --out ~/.local/share/kime/dict.bin
```

### 2. CLI 试打（REPL）
```bash
cargo run -p kime
# 或指定双拼方案
cargo run -p kime -- --shuangpin xiaohe
```

### 3. 切换输入法
```bash
kime-switch kime    # 停 fcitx5 + 启动 kime
kime-switch fcitx   # 切回 fcitx5
kime-switch status
```

### 4. 运行性能基准测试
```bash
cargo bench --bench kime_bench
```

## 开发约定

- **所有测试只在 PR CI 跑**（`ci.yml`：fmt check + `cargo test --workspace`）；本地只做 `cargo check` / `fmt --check` 轻量验证
- 测试放 `tests/` 目录，src 内严禁 `#[cfg(test)]`（githook 强制）
- 依赖单向：`bin/kime -> platform-wayland -> kime-core -> kime-shuangpin -> kime-pinyin`
- core 不碰任何显示/UI；壳只做「按键进、候选出、上屏提交」

## 词库说明

使用 [rime-ice](https://github.com/iDvel/rime-ice)（雾凇拼音）开源词库数据（GPL-3.0）。个人本地使用合规。
