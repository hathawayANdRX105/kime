# kime-typing-probe

离线 mock 敲字体验评测 crate，用于评估 kime 中英文拼音输入法的敲字体验。

## 设计目标

- 仅向下依赖 `kime-core` 与标准库（`serde` 用于报告序列化），不依赖 platform/render，不引入 `reqwest`（除非启用 `llm` feature）。
- 读取 TSV 语料，模拟按键，采集命中率/候选位置/翻页/回删/延迟/上下文命中。
- 输出 rules-only 或 LLM 可消费的 JSON/Markdown 报告。
- 所有测试离线运行，不依赖网络或用户真实词库；CI 无需联网。

## 目录结构

```
crates/kime-typing-probe/
  Cargo.toml
  src/
    lib.rs        // 公共 API：run() -> ProbeResult
    runner.rs     // 按键回放：Runner，逐键回放 KeyEvent，采样 Outcome/preedit/candidates/page/耗时
    corpus.rs     // 语料加载：TSV 行 (freq,text,pinyin)，按 zh/en 分桶
    metrics.rs    // 指标：target_hit_rate/top1_hit_rate/first_screen_hit_rate/latency p50/p99/page_turns/backspace/reselect
    report.rs     // 报告：JSON/Markdown，summary/per_lang/per_sentence/suggestions，rules-only 建议
    llm.rs        // LLM 接入：缺省不发网络；KIME_LLM_ENDPOINT 设置且开 llm feature 时可用，失败静默降级
  fixtures/
    zh_articles.tsv   中文语料
    en_articles.tsv   英文语料
  tests/
    typing_probe_test.rs  离线测试
  README.md
```

## 依赖与编译

`Cargo.toml`：

```toml
[dependencies]
anyhow = "1"
kime-core = { path = "../kime-core" }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

[dev-dependencies]
tempfile = "3"

[features]
default = []
llm = ["dep:reqwest"]

[dependencies.reqwest]
version = "0.12"
features = ["json", "blocking", "rustls-tls"]
optional = true
```

- 默认不启用 `llm` feature，`LlmClient::enabled()` 始终为 `false`，LLM 建议静默降级为 rules-only。
- 启用 `llm` feature 后，设置 `KIME_LLM_ENDPOINT` 环境变量即可让评测报告通过 LLM 生成改进建议；请求失败或端点缺失时不报错、不阻断。

## 使用

```rust
use kime_typing_probe::run;
use std::path::Path;

let result = run(Path::new("crates/kime-typing-probe/fixtures/zh_articles.tsv"),
                 Path::new("/tmp/kime-probe-dict.sqlite3"))?;
let json = result.to_json();
let md = result.to_markdown();
```

`run` 接受语料文件路径与**已存在的 sqlite dict 路径**（缺文件会新建空库，报告全零——
先 `kime --dict <db> --import <dict.yaml>` 准备好词库）。
按键序列由条目的读音生成（中文用 `pinyin`、英文用 `text`），逐字符输入后补空格提交。

## 排序层契约

与 kime-core 保持一致：中文精确 → 英文精确 → 中文补全 → 英文补全；上下文只层内重排。本 crate 不修改 core 任何排序逻辑，只消费公共 API（`Engine`、`Key`、`Outcome`、`Candidate`）。

## 环境变量

| 变量 | 说明 | 缺省 |
|------|------|------|
| `KIME_LLM_ENDPOINT` | OpenAI 兼容聊天端点，设置后启用 LLM 建议 | 未设置（rules-only） |
| `KIME_LLM_MODEL` | LLM 模型名 | `gpt-oss-120b` |
| `KIME_LLM_API_KEY` | API 密钥 | 无 |
| `KIME_LLM_TIMEOUT_SECS` | 请求超时秒数 | `5` |

## 测试

```bash
cargo test -p kime-typing-probe
```

所有测试离线，使用内嵌语料 + tempfile 临时 sqlite，不联网、不依赖用户词库。

## 报告格式

JSON 输出包含：

- `schema_version`、`generated_at`、`corpus_id`、`total_sentences`
- `summary`: target_hit_rate / top1_hit_rate / first_screen_hit_rate / latency_ms(mean,p50,p99) / page_turns / backspace_count / reselect_count / overall_score(0-100)
- `per_lang`: `{zh, en}` 各复制 summary 全套
- `per_sentence[]`: 逐句明细（target_hit / top1_hit / first_screen_hit / keys / latency_ms / page_turns / backspaces / reselections / candidates_sample）
- `suggestions[]`: rules-only 建议，LLM 启用且成功时覆写/追加

Markdown 输出为同结构的可读文本，适合直接贴入 PR 或文档。
