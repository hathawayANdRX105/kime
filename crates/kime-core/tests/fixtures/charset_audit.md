# 汉字集系统性审计报告（charset track，第五轮反馈第 2 条）

日期：2026-09-14 · 分支 `fix/charset` · 审计人：CharsetPunct

## 结论先行

- **单字缺字 = 0**：《通用规范汉字表》8105 字、GB2312-80 全部 6763 字，线上词库
  （`~/.local/share/kime/dict.sqlite3`，含 learner 行）逐字比对**无一缺失**，且每字至少有一个读音候选。
- 用户感到的「打某个读音出不来某个字」，病灶是上一轮的 **freq=0 压字 bug**（41448 抢先
  INSERT OR IGNORE 掉 8105 的真实频率，单字在 `(freq DESC)` 候选序里被整词压死，如「打 shi 出不来是」）——
  已修复；本次直接探活运行时 `dict.bin`（FST）：`shi → 1.是`、`hao → 1.好` 置顶正常。
- **读音级零实锤缺口**：kime 的音节键不带声调（`he` 通 hē/hé/hè/hé），所以多音字只要**字母形**在册即打通。
  20 个常见多音字全过（§3）；规范字一/二级表逐字「最常见读音」扫描 12 例报警全部为 pypinyin 噪声或
  生僻存疑（§4）。**没有任何一条需要为此重建词库的缺口。**

## 数据来源（全部只读，可复核）

| 数据 | 来源 | 版本/校验 |
|---|---|---|
| 线上词库单字 | `~/.local/share/kime/dict.sqlite3` `phrase WHERE length(text)=1` | mtime 2026-09-14 15:21；单字 40,596 字（含 user=1） |
| 运行时 FST | `~/.local/share/kime/dict.bin` | v3、mtime 09-12、探活通过（未触发 SQLite 回退） |
| 通用规范汉字表 8105 | https://github.com/frankslin/cn-characters-standard `data.js`（cdtm 数字化 xlsx，教育部 2013 表） | s=1..8105 逐行校验 |
| GB2312-80 全 6763 字 | Python 内置 `gb2312` codec 穷举区 16–87（A1–F7 × A1–FE）反解 | 恰得 6763，与国标字数吻合 |
| rime-ice 单字源 | 本地 clone `/home/hathaway/projects/rime-ice/cn_dicts/` | commit `fbb516b`（2026-08-31） |
| 常见读音参照 | pypinyin 0.55.0 Style.NORMAL + 人工裁决 | 假警报见 §4 逐条 |

## 1. 缺字清单（标准表有、词库单字无）

**空。** 按字表分组：

| 字表 | 表内字数 | 词库覆盖 | 缺失 |
|---|---|---|---|
| 通用规范汉字表（一/二/三级） | 8105 | 8105 | **0** |
| GB2312-80 | 6763 | 6763 | **0** |

（若只统计 `user=0` 导入行会**误报**缺 34 字——下 个 开 中 打 去 四 用 在 有 当 后 多 字 好 把 来 连 吧 我 你 况 补 词 英 到 树 要 点 是 排 做 就 跟：全部是 learner 提权假象，行仍在、可查，见 §5.3。）

## 2. 超出标准表的字（词库有、两表皆无）：32,366

几乎全部来自 `cn_dicts/41448.dict.yaml`（生僻字扩表）：

- CJK ExtA（U+3400–D4DF）：18,093
- CJK ExtB+（U+20000+，含 4 个私用区码位）：14,268
- 基本区但两表不收的 64 字：方言字/和制汉字/新化学元素/异体
  （㖏 㞎 㨃 䞍 丼 亖 冇 凃 卍 卻 咲 囧 奀 嬢 屄 屌 怹 挼 揾 朘 樋 氹 濛 炁 牠 珮 畑 睆 祂 罥 肏 脢 芔 苶 菈 蒾 蓺 薙 蟌 覅 觍 跩 車 辻 雫 霂 鞥 麿 鿔 鿕 鿫 鬬 鿭 𠲎 𤆵 𤭢 𪨊 𫚪 𫟷 𫪘 𭎂 〇 𰻝）——
  rime-ice 有意收录（含 2017 命名四元素字、biáng），**不是脏数据**。

评价：无害，只多占 dict.bin 体积（这些行 freq=0，不参与压顶序）。不建议删。

## 3. 多音字覆盖抽查（20 个常见多音字，字母形口径）

| 字 | 全部字母形读音 | 词库读音 | 判定 |
|---|---|---|---|
| 了 | le, liao | le, liao | ✅ |
| 和 | he(含 hè), hu, huo | he, hu, huo | ✅ |
| 重 | zhong, chong | chong, zhong | ✅ |
| 行 | xing, hang, heng | hang, heng, xing | ✅ |
| 长 | chang, zhang | cha, chang, zhang | ✅（cha 冗余无害） |
| 发 | fa | fa | ✅ |
| 还 | hai, huan | hai, huan | ✅ |
| 得 | de, dei | de, dei | ✅ |
| 为 | wei | wei | ✅ |
| 种 | zhong, chong | chong, zhong | ✅ |
| 中 | zhong | zhong | ✅ |
| 好 | hao | hao | ✅ |
| 数 | shu, shuo | shu, shuo | ✅ |
| 乐 | le, yue, yao, lao | lao, le, yao, yue | ✅ |
| 少 | shao | shao | ✅ |
| 相 | xiang | xiang | ✅ |
| 干 | gan | gan | ✅ |
| 朝 | chao, zhao | chao, zhao | ✅ |
| 传 | chuan, zhuan | chuan, zhuan | ✅ |
| 差 | cha, chai, ci | cha, chai, ci | ✅ |

（附带核对同样全过：曲 qu、系 ji/xi、曾 ceng/zeng、倒 dao、度 du/duo。）

## 4. 读音级缺口扫描（规范字一/二级表逐字，最常见读音 vs 词库）

pypinyin 单字首读音不在该字词库读音集内的共 12 例，逐一人工裁决：

| 字 | pypinyin 报警读 | 词库读音 | 裁决 |
|---|---|---|---|
| 芎 | qiong | xiong | ❌ 假警报：《现汉》川芎 chuān**xiōng**，词库对 |
| 饧 | tang | xing | ？生僻：táng 为「糖」异体读，可缓 |
| 呒 | wu | fu, mu | ？方言字（吴语 syllabic 鼻音，本就键不出） |
| 呣 | m | mou | ❌ syllabic /m̄/ 不在 416 音节表，rime 同无 |
| 珩 | hang | heng | ？人名读 háng 有辞书依据，可缓 |
| 菹 | ju | zu | ❌ 假警报：菹 = zū，pypinyin 误 |
| 豉 | shi | chi | ❌ 假警报：豉 = chǐ（豆豉），词库对 |
| 嗯 | n | en, eng | ❌ 同呣：syllabic 键不出；en/eng 已覆盖 |
| 嗲 | die | dia | ❌ 假警报：嗲 = diǎ，词库对 |
| 碡 | du | zhou | ❌ 假警报：碌碡 liù·zhou，词库对 |
| 阚 | han | kan | ❌ 假警报：姓阚 kàn，词库对 |
| 嬷 | ma | mo | ？mó 为主读，mà 存疑 |

**净结论：8 例假警报、4 例生僻候选（饧/呒/珩/嬷），一级字表零实锤缺读。**
若将来主控因其它原因重建词库，可顺手把下面 2 条带进补丁文件（都为二级表边缘字，不值得单独重建）：

```text
珩	háng	1
饧	táng	1
```

## 5. 根因分析

1. **历史病灶 = 频率压字（上一轮已修，本轮验证）**。rime-ice 设计中**单字只来自两张表**：
   `cn_dicts/8105.dict.yaml`（8,757 行带频率）与 `cn_dicts/41448.dict.yaml`（46,019 行无频率列）——
   `base/ext/others/tencent` 四表实测单字行 **= 0**（字在词中、不在单字表）。旧导入
   `INSERT OR IGNORE` + 文件名字典序让 41448（freq→0）压掉 8105 真实频率 → 全库 54% 行 freq=0 →
   单字被多音节词整体压出候选位（「打 shi 出不来『是』」即此）。现 `Dict::import` 已改「频率取大」。
   本轮复算：clone（2026-08-31 版）两张单字表全部行 vs 线上库 → **漂移 0 行**；FST 探活置顶正常。
2. **运行时新鲜度 = 唯一活跃风险**。壳层读 `dict.bin`（mtime 09-12），SQLite 由 learner 持续写
   （mtime 09-14），user=1 行走内存 overlay 不依赖 bin —— 当前一致。但**任何人改库后忘记
   `kime build-dict` 重烘 bin，用户就会看到旧库症状**——与本次「补全字表」的诉求正相关：
   补得再全，bin 不烘等于没补。
3. **learner 提权假象**：`learn` 把被选中的 `(pinyin, text)` 行置 `user=1`。34 个最常用单字的唯一
   单字行已被这样翻标（freq = rime 原值 + 按键数，如 的/de：76,938,354→76,938,355）。
   `import` 的 `ON CONFLICT ... WHERE user = 0` 守卫使其不再被词库更新 —— 这是**有意设计**
   （学习次数优先），行为正确；只有做「user=0 口径」的词库审计会误判缺字（§1 括号注）。
4. **上游（rime-ice）缺口**：在字母形口径下**未发现实锤缺口**（§3、§4）。41448 的 4 个私用区码位
   是上游数据小噪点，建议顺手向 rime-ice 提 issue（非阻塞）。

## 6. 重建词库待办清单（决策归主控；本审计不重建）

若重建，源文件缺一不可（本轮「汉字没做全」的怀疑对象经比对**都不是**漏项）：

- [ ] `cn_dicts/8105.dict.yaml` —— 规范字 + 频率（单字频率唯一来源）
- [ ] `cn_dicts/41448.dict.yaml` —— 生僻字扩表（freq=0）
- [ ] `cn_dicts/base.dict.yaml` / `ext.dict.yaml` / `others.dict.yaml` —— 词
- [ ] （可选）`tencent.dict.yaml`：`import` 会过滤其数字读法行；体积/收益由重建者定夺
- [ ] 英文：`en_dicts/en.dict.yaml`、`en_ext.dict.yaml` → `--import-english`
- [ ] §4 的 2 条边缘补丁（可选）
- [ ] **`kime build-dict --in <new.sqlite3> --out ~/.local/share/kime/dict.bin` —— 忘这步 = 白建**
- [ ] 探活：REPL `shi`→`1.是`；`hao`→`1.好`；双拼方案另测

不需要：补任何缺字（没有缺字）、删生僻字、改 `builder.rs`/`dict.rs`（导入侧已对全文件免疫顺序）。

## 7. 剩余风险

1. **bin 新鲜度流程**：词库→bin 是两步产物，无强制联动（§5.2）。建议把 import+build-dict 绑成
   同一条 just/脚本命令。
2. **繁体单字不在库**：`東` 这类独立码位繁体字的**单字**条目没有（繁体只随词进库）；打繁体属
   产品决策（rime 靠 `traditionalization` 开关），kime 现状一致。
3. **syllabic 鼻音读不出**：呣/嗯/唵 的 /m̄ n̄ ŋ̄/ 不在 416 音节表（与 rime 行为一致）。
4. 审计基于 09-14 线上库快照；用户继续学习会让 user=1 集合漂移。复跑前先
   `git -C /home/hathaway/projects/rime-ice pull` 对齐 clone。
