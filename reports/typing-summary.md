# 打字语料回放 · 基线（跨语言统一分数）

- 词库：`~/.local/share/kime/dict.sqlite3`（150 MB，含 dict.bin 复合层）
- 引擎：`fix/user-ranking`（跨语言按校准分数排序之后）
- 回放命令：

```bash
kime typing crates/kime-core/tests/fixtures/typing/<语料> \
          --dict ~/.local/share/kime/dict.sqlite3 --limit 200 \
          --out reports/typing-<语料>.md
```

- 语料（提交在仓里）：`crates/kime-core/tests/fixtures/typing/{zh_100,en_100,mixed}.txt`
- 指标：按**词**统计。`top1` = 候选首位；`top10` = 一屏内（page_size 10）；`miss` = 候选里没有。

## 排序口径（本次改动的核心）

排序键 `(tier, score DESC, text ASC, source)` —— **一个比较器，语言不是排序维度**：

```text
tier  = 匹配质量：精确 > 覆盖句 > 补全 > 尾部 > 兜底 > 追加
score = ln(1 + eff) × SCORE_MAX / ln(1 + max_eff_of_that_language)
```

- **层在分数之前** ⇒「精确命中整体在补全之前，哪怕补全频率高一个数量级」是结构性事实，
  任何频率、任何加成都跨不过去。
- **分数在每个语言自己的分布里归一化** ⇒ 中文与英文可比（原始数值 10⁵~10⁷ vs 10³
  直接比等于「中文恒赢」，只是语言分块换个说法）。
- 于是跨语言竞争发生在**同层之内**：同为精确层时按分数交错，同为补全层时同理。
- 任一方没有频率数据时退回原始频率比较（避免对数把不同量级压成同一个整数）。

### 为什么不「跨语言只比分」

试过「同语言比层、跨语言只比分」——看着更纯按分数，但它**不是全序**：
`(Zh,精确,10) < (Zh,补全,100) < (En,精确,50) < (Zh,精确,10)` 成环。比较器成环后
`sort_by` 的结果不再由输入唯一决定（同一多重集换个初始顺序就换个输出）。
「精确恒在补全之前」+「跨语言只比分」+「比较器可传递」三条不可兼得，这里保留第一条
（用户可感知），让跨语言竞争发生在同层之内。

## 结果

| 语料 | 词数 | top1 | top10 | miss | 报告 |
|---|---|---|---|---|---|
| zh_100（100 汉字） | 100 | 58 | 100 | 0 | `typing-zh_100.md` |
| en_100（106 英文词） | 106 | **47**（原 18） | **96**（原 37） | 5 | `typing-en_100.md` |
| mixed（58 汉字 + 6 英文词） | 59 | 32（原 30） | 56（原 55） | 3 | `typing-mixed.md` |

## 结论

### 1. 中文：候选可达性满分，首位命中率约六成

100 个汉字全部进候选（miss = 0），58 个占首位。没占首位的都是同音竞争
（`今` 在 `进`/`金` 之后），由频率决定。

### 2. 英文：切模式打英文从「不可用」变成「基本可用」

统一分数之前，英文词整体被推到 50 名之后（`the` 排第 70，top1 只有 18/106）。
改成跨语言按分数比较后：

- top1 从 18 涨到 **47**
- top10 从 37 涨到 **96**（106 个词里 96 个进首屏）

代价是**中文候选不再恒在英文之前**——这正是本次要的语义：谁分高谁在前。
例如真实词库上输入 `ru`：「如果」在第 3 位、`rug` 在第 27 位，因为
「如果」的校准分更高。

### 3. 仍 miss 的词与排序无关

- `chinese` / `english` / `kime`：真实词库的 english 表里没有这些条目；
- `a` / `a` / `a`：英文查询有「两字母起」的噪声闸（单字母不进候选）；
- `ru`（小写）：词库只有 `Ru` 一条（大写写法），小写没有条目。

这些是**词库内容**问题，不是排序问题；补词或改英文匹配策略是另一件事。

## 复现

```bash
# 1. 确定性回放（fixtures 词库，CI 也跑）
cargo test -p kime-core --test typing_corpus_test
cargo test -p kime-core --test debug_corpus_test

# 2. 真实体验（产出本报告）
kime typing crates/kime-core/tests/fixtures/typing/zh_100.txt \
          --dict ~/.local/share/kime/dict.sqlite3 --limit 200 --out reports/typing-zh_100.md
kime typing crates/kime-core/tests/fixtures/typing/en_100.txt ...
kime typing crates/kime-core/tests/fixtures/typing/mixed.txt ...

# 3. 单步排查（看某一步的层/语言/分数）
kime debug --dict ~/.local/share/kime/dict.sqlite3 --json r u
```