# kime 第五轮收尾：高频词调频（方案 B）+ 分词混排 — 实施报告

分支 `fix/freq`（HEAD 2d78bd8 之上，未 commit）。改动范围：`crates/kime-core/src/dict.rs`、
`crates/kime-core/src/lattice.rs`、新增 `crates/kime-core/tests/freq_boost_test.rs`。

---

## 1. 时间衰减调频（方案 B：使用即提升 + 时间衰减，rime user_freq 语义）

### 语义

```
effective_freq(user 词) = phrase.freq + (n − 1) × USER_BOOST × 0.5^(age_天 / 30)
USER_BOOST = 300_000        （常量，dict.rs）
半衰期     = 30 天
n          = 用户使用计数（含首次），age = 距最近一次使用的天数
```

- **独立空间**：用户使用计数 n 与语料计数完全分开；`phrase.freq` 一个字节没动
  （旧语义「词库频率 + 累计 bump 次数」），加成**只进比较器**，所有查询路径
  （lookup / lookup_prefix 两层 / merge_overlay / 块内预排）按 effective 排序，
  而 `Candidate.freq` 导出值恒为原始库频。
- **n=1 不加成是刻意的**：`tests/dict.rs::他(6) > 它(1)`（各学一次）与
  `user_overlay_test` 的 5001 断言钉死「学一次不得跳过语料词」。(n−1) 偏移让
  这些既有契约原样通过，从第 2 次选词起每次 +300k。
- **衰减是纯函数**：状态只有 (n, last_day)，无「推进窗口」的写放大——learn 只
  upsert 被选的那一行；开库一次全量 SELECT。跨重启、跨天自动生效。

### 持久化取舍：`kime_kv` 旁表（所选） vs freq 高位 vs abbrev 复用

| 方案 | 判定 |
|---|---|
| **kime_kv(key,value) 旁表**（`CREATE TABLE IF NOT EXISTS`，无迁移） | ✅ 选中。唯一不触碰任何既有读 freq 路径的方案：builder 打包、FST overlay 权威读回、`memory_vs_sql_consistency`（内存 index 逐行对 SQL）全部原样成立。几十行小表，learn 一次 UPSERT。 |
| freq 高位打包 (n<<40 \| base) | ❌ 污染一切：`SELECT freq` 的每个消费者（builder 全量导出、overlay 读回、top_user、全部钉值断言）都要解码；漏一处就把用户计数当语料频用。 |
| abbrev 列复用 | ❌ abbrev 是声母前缀查询的检索键（有索引 `idx_phrase_abbrev`），改写即坏。 |
| 每词一列 / ALTER TABLE | ❌ 改 schema，工单明令禁止。 |

### FST 模式正确性

learn 的 user_overlay 同步分支（UPDATE 命中 user 0→1 时从库读回权威 freq）逐行未动——
它读回的是**原始** freq，加成在 `merge_overlay`（改为 `&self` 方法 + effective 比较器）
上叠加。`user_overlay_test` 3/3 绿，lib 内 200 次 learn 置顶的 composite 测试绿。

### BOOST 校准数字

- 语料基线：本库 `SUM(freq)=7.53e9`、92.9 万行；「我们」=509,405，「在」=19,195,989，
  「再」=2,681,503，精确 `wo'men'zai` 词条只有 我们在=100 / 我们再=12。
- n=2 → +300k：压过 10⁵ 级语料词（我们 100 直接让位，见下方 REPL 实测）。
- n=3 → +600k：压过 5×10⁵ 级普通词（freq_boost_test 的钉值：500,000 的「我们」被让位）。
  工单建议 200k 是按 `n×B` 口径；(n−1) 偏移保持下，取 300k 使「第 3 选压过 50 万」
  语义原样成立。n=4 → 900k。
- 停用 30 天减半、90 天 n=3 加成 600k→75k（回落 500k 词之后——测试断言）、
  600 天归零。

## 2. 分词混排（4 音节：整句与 2+2 组合同列，频率高者在前）

### 实测先推翻了对病灶的假设

`sentence_score` 的 `4.60517×extra` 项 extra=`max(0,k−2)`：**k=2 的 2+2 组合本来就不吃
×1/100 惩罚**。真正让「我们再」消失的是 **`PATHS_PER_NODE=2`**：词库存在整串词条时其
路径概率代价恒排第一，dp[终点] 的两个名额被整句 + 一个任意次优路径占掉，2+2 组合
**根本进不了候选**（实测 `womenzai` 的「我们+再」整条被剪）。

### 改动（只动常量/公式，不动拓扑）

1. `PATHS_PER_NODE: 2 → 3` —— 2+2 组合获得名额；k≥3 碎切不受名额红利（分数照样 ÷100/词，
   见下）。
2. **viterbi 边权改用 effective_freq**（`dict.effective_freq(best)`）：用户刚用过的词把
   包含它的整句/组合抬上去，停用则随半衰期自动落回。排序键与 lookup 候选序同一量，
   杜绝「查询层置顶、组句层隐形」。
3. 其余公式原样：代价序（概率）仍钉 k 条路径内的名次——`知道` 不被 `知+道` 顶掉
   （lattice/engine/sentence_ranking 三处钉死），`place_sentences` 按 `sentence_score`
   在补全区**按频率落位**——这本来就是混排，实测确认（1622 的组合排在 396 的组合前、
   100 级补全词前，6935 的精确整串真词在它们前）。

### 前后对比（真实线上词库副本，`target/debug/kime repl`，用户自己的 ziranma 配置）

```
BEFORE（HEAD 2d78bd8）：
womendezai: 1.我们的在 2.我么娘欸爱
tiankongzhicheng: 1.天空之城 2.天空支撑
womenzai: 1.我们在 2.我么内爱 3.我们再 4.我闷在 5.我们再也回不去了 … 14.我们在长大
wohenxiangquni: 1.我很想娶你 2.我很想去你 3.我和捏差能去你 4.我和捏差能娶你
bucuobao: 1.不错报 2.不错保 3.不粗欧傲

AFTER（本改动；同一库里用户对「我们再」选词 8 次后又把计数拨旧 900 天——衰减态）：
womendezai: 1.我们的在 2.我么娘欸爱                       ← 不变（无竞争候选）
tiankongzhicheng: 1.天空之城 2.天空支撑 3.天控制成          ← 第 3 条 2+2 组合现身
womenzai: 1.我们在 2.我们再 3.我闷在 4.我们再也回不去了 … 14.我么内爱
wohenxiangquni/bucuobao: 不变
```

关键变化：真词「我们再」从第 3 升到第 2（衰减态下仍超过全部补全、紧贴同音真词按裸频
排序），双拼碎切垃圾「我么内爱」从第 2 沉到第 14。

### 调频前后对比（探针 `examples/freq_probe`，全拼、关双拼，导出 freq 原值可见）

```
BEFORE（旧代码，对「我们再」连选 8 次）：
pickword womenzai 我们再 x8: #1->我们再 ×8            ← 8 次使用后仍卡第 2
dump womenzai: 我们在[108] 我们再[20] 我闷在[1411] 我们再也回不去了[100] …
（旧 +1 语义：freq 12→20，排序零变化——工单要消灭的现状）

AFTER（新代码，同脚本）：
pickword womenzai 我们再 x8: #1 #1 #0 #0 #0 #0 #0 #0  ← 第 2 次(n=2,+300k) 立刻顶到首位
dump womenzai: 我们再[20] 我们在[100] …                ← 排序变了，导出 freq 仍是裸值
decay 90:  我们再[20] 我们在[100]                     ← n=8、3 个半衰期后加成 7×300k/8≈262k
                                                          仍高于裸频 100 的对手（校准正确）
decay 900: 我们在[100] 我们再[20]                     ← 加成衰减到 0，排序回落基线 ✓
```

（对 50 万级语料词的 90 天回落 = freq_boost_test 的硬断言，真实库对手词频太小不适合演示。）

## 3. 验收与证据

- `cargo clean -p kime-core && cargo test --workspace --offline` → **全绿，exit 0**
  （40+ 个测试目标；kime-core lib 62 + 23 个集成测试目标，含点名保护的
  common_words(252 词×4 路径)/sentence_ranking/english_layer_one/user_overlay/topk/merge_layers）。
- 新增 `crates/kime-core/tests/freq_boost_test.rs`（2 用例）：
  ① n=1/n=2 不超 50 万词 → n=3 压过（钉 BOOST 校准）+ 导出频率恒 13 + **重开库排序保持** +
  **拨旧 90 天（3 半衰期）排序回落** + 回落后导出频率仍 13；
  ② 前缀层一内 n=3 置顶（learn 的块重排路径）。
- 变异证据（每次单点改动，跑相关测试集后还原）：
  - 去掉 (n−1) 偏移 → `freq_boost_test::boost_calibrates… FAILED`（钉住「首选用词不跳位」）；
  - 衰减因子改常量（`powf(0.0)`，永不衰减）→ 同测试 FAILED（钉住衰减回落）；
  - 还原后全绿。freq_boost_test 连跑 3 次稳定（首版有一处测试自身 /tmp 种子文件争用的
    flake，按测试名后缀分离后消除）。
- `cargo fmt -p kime-core` 干净（`--check` 无 diff）。未 commit（遵守工单）。

## 4. 剩余风险（明知而未做）

1. **`place_sentences`/engine 去重合并仍用裸 freq 比较**：boosted 用户词在**同层内**置顶，
   但与整句分数的跨列插位不看加成。用户词的 boost 经 lattice effective 已进入整句分数，
   残余偏差小；要彻底统一可把 place 的扫描也走 `dict.effective_freq`。
2. top_user 按裸 freq 排（个性化列表语义未调频）——被 `dict_top_user_returns_only_user_rows`
   的 [你好, 它] 顺序钉死，保持原样。
3. 常驻进程跨天不 learn 则 `today` 缓存不刷新 → 当晚 boost 少衰减 ≤1 天（≤2%/半衰期）。
4. 升级前已学的老用户词没有使用计数（历史 bump 与语料频率不可区分）→ n 从 1 重计，
   第二次使用起正常提升。行为=从头开始，不炸。
5. 双字组合「之道」类风险维持现状：列表名次由概率代价钉（与 HEAD 一致），boost 只
   放大用户主动选择的词。
6. `examples/freq_probe.rs` 为一次性探针（工单验收 3 的采集工具），已在验收后 trash，
   不入库。

## 5. 主控审查（CRG + code-reviewer）记录与修复

**CRG**（`.wt/freq` 内独立 graph）：update 3 文件 / 17 函数 / 0 affected flows，
风险 0.40；test gap 列出的 `today_days`/`user_bonus_of`/`entry_eff_freq`/`entry_cmp`
均为纯函数，由 `freq_boost_test` 端到端覆盖（断言排序与衰减回落）。

**发现并修复 1 个真实缺陷**：
- `Dict::lookup_abbrev` 的 SQLite 回退分支（`dict.rs:844-869`）原先按
  `abbrev_index` 的裸 freq 块序直接截断，**不吃提频加成**——FST 分支走
  `merge_overlay`（已用 `cand_cmp`）而 SQLite 分支没有，两条路径行为分裂：
  用户打缩写（如 `wm`）时「使用频率高的在前」不生效。
- 修复：SQLite 分支在 truncate 前按 `self.cand_cmp` 重排，与 FST 分支同语义。
- 回归测试 `boost_applies_to_abbrev_lookup`（`freq_boost_test.rs`）：种子
  `wm` → 我们(500000)/我温(10)，learn「我温」3 次后缩写查询置顶；修复前该
  测试 FAILED（排序未变），修复后 PASSED。

**编辑工具事故（过程记录）**：相对路径的 edit 曾解析到主仓库而非 worktree，
  污染了主仓库 `dict.rs`（已 `git checkout --` 恢复，主仓库验证干净）；
  一次 `PUT 864.=864` 误把 `candidates.truncate(limit)` 替换掉，导致
  `lookup_abbrev("w", 1)` 返回 2 条候选，被既有测试
  `dict_lookup_abbrev_matches_prefix_and_orders_by_freq` 抓出。补回 truncate
  后全绿。教训：worktree 内一律用绝对路径编辑。

**复验**：`cargo test --workspace --offline` 全工作区 0 失败
（kime-core 62 lib + 24 集成目标，freq_boost_test 3/3）；`cargo fmt -p kime-core
-- --check` exit 0；主仓库与 worktree 状态符合预期（改动只在 `.wt/freq`）。
