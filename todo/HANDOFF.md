# kime 交接书 — 候选词优化轮收尾（2026-09-15）

> 写给下一个会话：本文件是第五轮（用户调频 + 分词混排）与第六轮（上下文感知候选）
> 收尾时的全量交接。REPORT.md 已删除，其完整内容可用
> `git show b8d06de:REPORT.md` 从 git 历史找回。

## 1. 背景

- **项目**：kime — 个人向 Rust 拼音/双拼输入法，Linux Wayland 优先（mangowm 实机）。
- **会话链**：本交接承接 omp 会话 `01a0a023-6479-7213-9665-a29f8e10e2aa`
  （2026-09-14 13:37 → 09-15 19:10，transcript 在
  `~/.omp/agent/sessions/-projects-kime/`）。该会话断点续接自 `01a0a019`。
- **用户原始诉求**：「4 词时候选应该是混在一起，使用频率高的在前」——打过的词要置顶，
  整句与 2+2 组合要混排，不用过的碎切垃圾要沉底。
- **方案拍板**：方案 B（使用即提升 + 时间衰减，rime user_freq 语义）+ 分词混排。

## 2. 已完成（全部在 main，勿重复实现）

| 内容 | 落点 | 证据 |
|---|---|---|
| 用户调频方案 B：`effective_freq = phrase.freq + (n−1)×300_000×0.5^(age_天/30)`，30 天半衰期，n=1 不加成 | `crates/kime-core/src/dict.rs`（`cand_cmp` / `merge_overlay` / learn upsert `kime_kv` 旁表） | commit `b8d06de`（PR #23） |
| 分词混排：`PATHS_PER_NODE 2→3`，viterbi 边权改用 effective_freq | `crates/kime-core/src/lattice.rs` | 同上 |
| `lookup_abbrev` SQLite 分支修复（原不吃提频，与 FST 分支行为分裂） | `dict.rs:844-869` 一带，truncate 前按 `cand_cmp` 重排 | 同上 + 回归测试 `boost_applies_to_abbrev_lookup` |
| 上下文感知候选：surrounding_text 四事件、种子先验、ai_realtime 默认关 | platform-wayland + `kime-core/src/engine.rs`/`config.rs` | commit `2a1bedf`（PR #24） |
| 上下文收尾修复：Shift-hold 标点、done-batch 日志、bench 名修正、CI libxcb | `d767257` `fc2cbde` `8defa90` `363809a` `568d172` | git log |
| 回归保护 | `crates/kime-core/tests/freq_boost_test.rs`（BOOST 校准 / 90 天回落 / 前缀层一置顶 / 缩写提频） | 测试通过 |

- **验收基准**：全工作区测试全绿（`cargo test --workspace`，kime-core lib 62+ 集成目标）；
  CRG 风险 0.10–0.40；REPL 实测 `womenzai`：「我们再」第 3→第 2，「我么内爱」第 2→第 14。
- **REPORT.md 已删**（本轮任务 2），事实已并入 README/ROADMAP；需要细节走 git 历史。

## 3. 残余问题（明知而未做，低危）

1. **`place_sentences` / engine 去重合并仍用裸 freq**：boosted 用户词同层内置顶，
   但与整句分数的跨列插位不看加成。残余偏差小（boost 已经 lattice effective 进入整句分）。
2. **top_user 按裸 freq 排**：个性化列表语义未调频，被
   `dict_top_user_returns_only_user_rows` 的 [你好, 它] 顺序钉死。
3. **常驻进程跨天不 learn 则 `today` 缓存不刷新**：当晚 boost 少衰减 ≤1 天（≤2%/半衰期）。
4. **升级前已学的老用户词没有使用计数**（历史 bump 与语料频不可区分）→ n 从 1 重计，
   第二次使用起正常提升；行为=从头开始，不炸。
5. **双字组合「之道」类风险维持现状**：名次由概率代价钉死（与历史一致），
   boost 只放大用户主动选择的词。

## 4. 难题 / 下一会话候选词优化方向（硬问题）

1. **比较器全路径统一 effective_freq**：把 `place_sentences` 扫描与 top_user 也切到
   `dict.effective_freq`，消灭「查询层置顶、组句层/列表层隐形」的最后一处分裂。
   注意：top_user 语义变更会动 `dict_top_user_returns_only_user_rows` 断言，需先拍产品语义。
2. **PATHS_PER_NODE 增大下的碎切抑制**：名额红利只该给 2+2；k≥3 碎切目前靠
   `sentence_score` 的 ×100/词 概率代价压制，若继续加 PATHS 需重新验证
   `sentence_ranking` / lattice / engine 三处钉值。
3. **衰减函数长期稳定性**：`0.5^(age/30)` 纯函数无写放大，但缺长期观测；
   `freq_boost_test` 的 90 天回落断言是唯一护栏。900 天归零已实测。
4. **sentence_score 与 effective_freq 的跨层一致性**：排序键与组句权已同一量，
   但补全区落位（place_sentences）是裸频——与问题 1 同根。
5. **XWayland 上下文边界**：微信/QQ 无 text-input，光标不跟随是协议边界不是 bug，
   长期解是 M12 的 XIM 前端。

## 5. 环境与验证约定（新会话必读）

- 主仓库根目录只读；子任务开 `.wt/<name>` worktree（`git worktree add .wt/<name> -b <branch>`）。
  **worktree 内编辑一律用绝对路径**（本轮曾发生相对路径污染主仓库的事故，已恢复）。
- 本地只 `cargo check`；全量测试放 PR CI。bench 需套 `cpulimit -l 65 -i --`。
- 编译/测试一律 `cpulimit -l 65 -i -- cargo <cmd>`。
- 实机测试前停 fcitx5：`systemctl --user stop 'dbus-:1.2-org.fcitx.Fcitx5@1.service'` +
  `pkill fcitx5`；结束后 `setsid fcitx5 -d` 恢复。严禁全屏测试窗口。
- 本地 192 万词条库：`~/.local/share/kime/dict.sqlite3`；FST 词库
  `~/.local/share/kime/dict.bin`（改动 builder 后需重建）。
- 删除文件一律 `gio trash`；VCS 跟踪文件可用 `git rm`。

## 6. 文档状态

- `README.md`：功能表已含用户调频 / 4 音节混排 / 上下文感知 + 已知边界一行。
- `ROADMAP.md`：M10（候选词优化 ✅ 09-14）、M11（上下文感知 ✅ 09-15）、M12 待规划
  （XIM / 多模式 / 云同步 / 调频续篇）。
- `REPORT.md`：已删，`git show b8d06de:REPORT.md` 可找回全部技术细节与前后对比数据。
