# 打字语料回放 · 基线

- 词库：`~/.local/share/kime/dict.sqlite3`（150 MB，含 dict.bin 复合层）
- 引擎：`fix/user-ranking` @ `a9cb6eb`（候选模型/排序内核/用户统计统一之后）
- 回放命令：

```bash
kime typing crates/kime-core/tests/fixtures/typing/<语料> \
          --dict ~/.local/share/kime/dict.sqlite3 --limit 200 \
          --out reports/typing-<语料>.md
```

- 语料文件（提交在仓里，可复用）：
  `crates/kime-core/tests/fixtures/typing/{zh_100,en_100,mixed}.txt`
- 指标口径：按**词**统计。`top1` = 候选首位；`top10` = 一屏内可见（默认 page_size 10）；
  `miss` = 候选里完全没有。

## 结果

| 语料 | 词数 | top1 | top10 | miss | 报告 |
|---|---|---|---|---|---|
| zh_100（100 汉字） | 100 | 58 | 100 | 0 | `typing-zh_100.md` |
| en_100（106 英文词） | 106 | 18 | 37 | 5 | `typing-en_100.md` |
| mixed（58 汉字 + 6 英文词） | 59 | 30 | 55 | 3 | `typing-mixed.md` |

## 结论

### 1. 中文：候选可达性没问题，首位命中率约六成

100 个汉字**全部**出现在候选里（miss = 0），其中 58 个能直接占据首位。
没占首位的典型是单字同音竞争（`今` 排 `进`/`金` 之后，`输` 排第 7），这是频率决定的，
不是顺序 bug。

### 2. 英文：在中文模式下打英文**不可用**，这是当前策略的直接后果

106 个词只有 18 个进首屏、5 个完全不在候选里（`chinese` / `english` / `a` / `a` / `a`）。
原因不是英文查询坏了，而是「中文候选整体在英文之前」这条用户裁定的策略：
中文块先占满 50 个名额，英文块整体被推到 50 名之后，`the` 因此排在第 70 位。

结论：**英文输入必须走英文模式（Shift）**，或给「中文模式下的英文候选」单独的名额策略。
这是策略层面的取舍，不是排序缺陷——但它意味着「不切模式直接打英文」这条路在当前
设计下不成立，需要明确写进文档，或者提供一个可配置的名额下限。

### 3. 混合文本：中文部分正常，英文词同样沉底

59 个词里 30 个首位、55 个进前 10；未命中的 3 个都是英文词（`kime` / `english` / `ru`）。
`ru` 未命中是因为真实词库只有 `Ru`（大写写法），小写 `ru` 没有条目——
英文匹配是大小写不敏感查询但按词库写法回文本，条目不存在时自然没有候选。

## 复现步骤（改代码前后各跑一次）

```bash
# 1. 确定性回放（fixtures 词库，CI 也跑这条）
cargo test -p kime-core --test typing_corpus_test

# 2. 真实体验（真实词库，产出本报告）
kime typing crates/kime-core/tests/fixtures/typing/zh_100.txt \
          --dict ~/.local/share/kime/dict.sqlite3 --limit 200 \
          --out reports/typing-zh_100.md
kime typing crates/kime-core/tests/fixtures/typing/en_100.txt ... 
kime typing crates/kime-core/tests/fixtures/typing/mixed.txt ...

# 3. 单条候选排查（看清某一步的层与语言）
kime debug --dict ~/.local/share/kime/dict.sqlite3 --json n i h a o
```

## 下一步候选（未做，需决策）

1. 给英文块一个**最小名额**（例如无论中文多少，英文至少占 1–2 个位置），
   否则「中文模式直接打英文」永远不可用。
2. 英文语料改用英文模式回放（Shift 直通），单独统计「切模式成本」。
3. 把 top1/top10 做成 CI 阈值（需先确认 fixtures 词库上的稳定值）。