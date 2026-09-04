# kime 路线图

原则：每步都竖切——每一期结束都有一个「真的能跑」的东西，不横着铺层。

## M1 — 引擎最小闭环（纯逻辑，无 UI 无协议）

`kime-pinyin` + `kime-core`，零平台依赖。

- [x] 拼音音节切分：~410 合法音节表，DP 切分（`xian` → xi'an / xian 两义都出）——401 条，长优先 DFS
- [x] 词库 loader：rime-ice `.dict.yaml`（TSV 体）→ SQLite
  - 表：`phrase(pinyin, text, freq, abbrev)`，pinyin=音节带 `'` 连接，abbrev=声母缩写（nh → n'h）
  - 双索引 + LIMIT N by freq
- [x] 查询：精确 + 前缀，top-N（`Dict::lookup` / `Dict::lookup_prefix`）
- [x] kime-cli REPL：stdin 逐行输入 → 打印 top-10 候选（本期的可运行检查）

验收：`cargo test`（切分/loader/查询）+ kime-cli 出候选。
依赖检查：rime-ice license 先核。

## M2 — 双拼 + 学习

- [x] 双拼码表（kime-shuangpin）：小鹤、自然码 → 转拼音走同一管线（纯查表）——401×2 全量 round-trip
- [x] 用户词/词频自学习：选词写回 `phrase`(user=1)，bump freq 影响排序
- [x] 自定义缩写映射（个人习惯短语）——`Dict::lookup_abbrev` 兜底查询；自定义词条插入待 M4（需 UX 决策）

验收：单元测试覆盖码表 round-trip + 词频 bump 改变排序；kime-cli 里双拼键出拼音候选。

## M3 — Wayland 前端：第一次真上屏 ⚠️ 风险最高

`platform-wayland`：wayland-client + **wayland-protocols-misc（`input_method_v2`，已核实存在）**

- [ ] spike（先做）：mango 上 hello-world —— grab keyboard，按啥 commit 啥
- [ ] 完整通路：grab → 键盘事件进 core → preedit 显示拼音串 → 候选窗（layer-shell + cosmic-text 渲染）→ 数字/空格选词 → commit_string 上屏
- [ ] 英文直通模式；systemd user unit 自启

已知边界：XWayland 应用不支持（input-method-v2 只管原生 wayland），明确接受。
风险：mango 对协议实现成熟度、cosmic-text CJK 渲染——都在 spike 阶段暴露。

验收：真机 mango 会话往 GTK/Firefox 打中文。

## M4 — 日常体验补全（列难受清单，逐个修）
- [ ] 候选翻页（`-=`/`[]`）、退格删音节重切分
- [ ] 模糊音（配置化）

## M5 — AI 预测（异步第二梯队，可选）

- [ ] 停顿 ~300ms 或句尾时，上下文发本地 LLM（OpenAI 兼容端点），整句/候选合入列表
- [ ] 硬约束：主路 P99 < 20ms，AI 永不阻塞按键
- [ ] AI 候选被选 → 写回 SQLite 喂频率

## M6 — 平台壳（远期，要用才做）

XIM / Windows TSF（参考 Weasel）/ macOS IMKit（参考 Squirrel，IMKCandidates 白送 UI）。
