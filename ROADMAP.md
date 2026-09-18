# kime 路线图

原则：每步都竖切——每一期结束都有一个「真的能跑」的东西，不横着铺层。

## M1 — 引擎最小闭环（纯逻辑，无 UI 无协议）✅ 2026-09-05

`kime-pinyin` + `kime-core`，零平台依赖。

- [x] 拼音音节切分：404 合法音节表，长优先 DFS 切分
- [x] 词库 loader：rime-ice `.dict.yaml`（TSV 体）→ SQLite
- [x] 查询：精确 + 前缀，top-N（`Dict::lookup` / `Dict::lookup_prefix`）
- [x] CLI REPL：stdin 逐行输入 → 打印 top-10 候选

## M2 — 双拼 + 学习 ✅ 2026-09-05

- [x] 双拼码表（kime-shuangpin）：小鹤、自然码 → 404×2 全量 round-trip
- [x] 用户词/词频自学习：选词写回 `phrase`(user=1)，bump freq 影响排序
- [x] 自定义缩写映射：`Dict::lookup_abbrev` 兜底查询

## M3 — Wayland 前端：第一次真上屏 ✅ 2026-09-05

`platform-wayland`：wayland-client 0.31 + `wayland-protocols-misc`（`input_method_v2`）

- [x] spike：mango 上 hello-world —— grab keyboard，按啥 commit 啥
- [x] 完整通路：grab → 键盘事件进 core → preedit 显示拼音串 → commit_string 上屏
- [x] 协议纠错：`commit(serial)` 的 serial 严格对应 done 事件计数

## M4 — 候选窗渲染管线（wl_shm + cosmic-text）✅ 2026-09-06

- [x] 真 wl_shm/memfd 渲染管线：buffer 与 mmap 同源，Argb8888，stride=w*4
- [x] cosmic-text 渲染 CJK 字体，实底深灰蓝底 + 白字
- [x] layer-shell 独立窗实验废弃：wayland 客户端无权自行摆位，候选必须走 input-popup

## M4.5 — 候选窗进 input-popup-surface（光标跟随的正解）✅ 2026-09-12

- [x] 候选词直接画进 `zwp_input_popup_surface_v2`：attach + commit 才 mapped，合成器用 text-input positioner 摆位；独立面板进程 / socket / IPC 搬窗全部移除
- [x] 横排单行候选、宽度随内容自适应、不画 preedit；隐藏 = 提交 1×1 全透明帧，surface 全程不销毁
- [x] `text_input_rectangle.height` 仅作光标行占位，摆位交合成器；双缓冲 + frame 回调控帧

## M5 — 翻页 + 模糊音 + 音节补全 ✅ 2026-09-06

- [x] 翻页：`-`/`[` 上一页、`=`/`]` 下一页；数字键当前页内选词
- [x] 模糊音：`config.fuzzy: ["n=l", "an=ang"]` 配置化，声母/韵母独立替换
- [x] 音节补全：+`yo`/`dia`/`lo` → 404 全量音节
- [x] 根因修复：abbrev 兜底不再覆盖正常前缀结果

## M6 — 词库升级 + 查询性能优化 ✅ 2026-09-06

- [x] rime-ice 六分表全量导入：**1,921,611 词条**（185MB SQLite）
- [x] 范围查询 `[lower, upper)` 替代 `LIKE`：查询从 41.66ms → **0.18ms**（231 倍加速）

## M6.5 — 内存双排序索引 ✅ 2026-09-06

- [x] 按键热路径脱离 SQL：主索引 (pinyin, freq, text) + 二级索引 (abbrev, freq, text)
- [x] 内存二分查找：`ni'hao` **0.007ms**、`shen'me` **0.305ms**、`abbrev nh` **0.089ms**

## M7 — FST 二进制词库（mmap）✅ 2026-09-06

- [x] `builder.rs`：SQLite → dict.bin（4.7s 处理 192 万词条）
- [x] `store.rs`：`FstStore` mmap 加载，内存从 **228.8MB → 41.7MB**（缩减 81%）
- [x] CLI `build-dict` 子命令：单条命令完成词库编译
- [x] 持久化基准测试：`cargo bench --bench kime_bench`

## M8 — 日常体验完善与分词进阶 ✅ 2026-09-06

- [x] 中文标点符号映射与顶字上屏（`punct.rs`，全角符号自动转换与 preedit 顶字）
- [x] TOML 配置文件解析与初始化（`~/.config/kime/config.toml`，自动生成与容错降级）
- [x] FST 复合存储与用户词 SQLite Overlay（42MB mmap 只读基底 + 本地生词/提频动态合并）
- [x] 候选窗主题样式：背景 (30,30,38) 实底、高亮琥珀、横排单行宽度随内容自适应（逐候选实测字宽）
- [x] 句级 Word Lattice 构建与轻量 Viterbi 动态规划联想（第一候选输出长句）

## M9 — 进阶功能 ✅ 2026-09-06

- [x] 中英标点切换（`Ctrl+.` 切换，`Config.punct_mode` 字段）
- [x] 托盘图标与状态指示（`kime status` CLI + `TrayIconManager`）
- [x] LLM 联想增强（`Engine::merge_ai()` + `LlmClient` + Debouncer）
- [x] 默认双拼自然码 + CLI `config` 子命令热更新
- [x] 翻页键增强（`+`/`=` 下一页，`Ctrl+f/b/n/p` Emacs 风格）
- [x] Enter 原样上屏 + 数字 0 选第 10 个候选

## M10 — 候选词优化：用户调频 + 分词混排 ✅ 2026-09-14

- [x] 用户调频（方案 B）：`effective_freq = phrase.freq + n×300_000×0.5^(age_天/30)`，30 天半衰期，首用即满额（2026-09-15 用户拍板，原 n=1 零加成契约废止）；计数存 `kime_kv` 旁表（独立空间，导出 freq 恒为库内原值），FST/SQLite/缩写三条查询路径统一比较器
- [x] 分词混排：`PATHS_PER_NODE 2→3`（2+2 组合进候选），viterbi 边权改用 effective_freq；实测 `womenzai` 真词「我们再」从第 3 升至第 2，双拼碎切「我么内爱」沉到第 14
- [x] 回归保护：`freq_boost_test`（BOOST 校准 / 90 天回落 / 前缀层一置顶 / 缩写查询提频）

## M11 — 上下文感知候选 ✅ 2026-09-15

- [x] input-method-v2 `surrounding_text` 四事件捕获（双缓冲 + done 提交，`cause=INPUT_METHOD` 回声过滤）
- [x] 上文末词 `readings_of_text` 反查作 lattice 虚拟起点（种子先验，不进产出文本）
- [x] AI 实时候补默认关闭（`ai_realtime = false`）
- [x] 已知边界：XWayland 应用无上下文 → 优雅退化；调频加成未跨列（见 M10）

## M12 — 输入体验：邻键纠错 + 双拼半截键补全 ✅ 2026-09-15

- [x] 全拼邻键纠错：按键串层编辑距离 1（QWERTY 邻键替换 + 相邻转位），直查候选 < 5 才触发（正常输入零开销），纠错候选恒排精确结果之后（`config.correction`，默认开）
- [x] 双拼半截键补全：零声母键（y/w/元音）公共前缀塌缩 → 枚举完整音节、末音节匹配词置顶；`ke`+`y` → ke'yi 词在前，单键 `y` → yi 系词在前（有公共前缀的键 u→sh 行为不变）
- [x] 回归：`correction_test`（转位/替换/充足不纠错/关闭不纠错）+ `shuangpin_tail_test`（key 补全/单键简拼/偶数键不变）

## M13 — 性能轮：有效频率烘焙 + 纠错门控 ✅ 2026-09-15

- [x] **eff 烘焙进条目**（`IndexEntry.eff` / `Candidate.eff` = 裸频 + 用户提频，开库/learn 每用户行一次 powf）：比较器变纯字段比较（消灭每次比较的 String 分配 + 哈希 + powf），Viterbi 整句 7 音节 **7.5 倍**（0.28→0.037ms）
- [x] 删开库/导入时对已 `ORDER BY` 结果的全量重排；boost 改为查询时按 `user_pinyins` 门控的块内重排（`lookup_prefix` 层一此时先取整块再截断）
- [x] **纠错门控**（修 20x 回归）：`kime_pinyin::is_fully_segmentable` 零分配预筛变体 + `MAX_CORRECTION_INPUT=12` 字母长度门控（长句容错归 Viterbi）；27 键长句最慢键 4.4→0.29ms
- [x] `examples/typing_probe` 逐键探针（qingjian `--typing` 同款：逐键增量才是真实负载）+ CI `perf` 作业（合成 10 万词库基准进 job summary，continue-on-error）
- [x] 判定记录：rayon **不加**（每键微秒级，调度反噬）；FST 层二 shen'me 慢 10 倍 = 1355 续接 key 的剪枝线性扫（算法地板，不动）；eytzinger 不适用（kime 要返回区间位置，换算成本反转收益，algorchemy doc 实测）

## M14 — 词图格子缓存（SpanCache）✅ 2026-09-15

- [x] lattice 跨度查词缓存：键 = 跨度拼音串，值 = `Arc<[Candidate]>`；Engine 持有，`learn_or_warn` 尾部整体失效（learn 改条目 eff），8192 上限整清重建
- [x] 验收：`span_cache_test` 4/4（缓存命中 vs 全冷对拍 / learn 失效 / 前缀延长增量复用 / 上限）+ 33 套件全绿 + typing_probe 27 键长句平均 0.088ms（无回退）
- [x] 审查：CRG 0 affected flows（risk 0.40）+ 结构化 8 项清单 PASS_WITH_NITS，记录在 PR #25 评论
- [ ] 远期：双层词库（热层 FST 常驻堆 + 冷层 mmap 按需）——词库到千万级时的内存扩展性保险

## M16 — 剪贴板候选（第一阶段）✅ 2026-09-16

- [x] 动态历史：`wl-paste --watch` 后台监听（wl-clipboard 已是机内依赖，无新 crate），`ClipStore` 有界 64 条、同文去重置顶、单条 4096 字符截断（防超大复制拖挂渲染）
- [x] 用户预设：只读加载 deskctl snippets 目录（`~/.config/deskctl/snippets/<topic>/<template>`），与 deskctl 面板同源，一处维护两处可用
- [x] 壳内交互：`C-;`（evdev 39）进/出剪贴板模式，j/k 导航、Enter/空格上屏、Esc/字母退出并转交引擎；路由决策抽纯函数 `clip_route`（route.rs，12 用例离线钉行为），副作用（重绘/提交/记账）归 main.rs 执行层
- [x] 隐私与配对：剪贴板文本不落 `/tmp/kime-ime.log`（提交走无日志投递路径）；clip 模式吞键记入 SwallowTracker，release 配对不漏
- [x] 验证：kime-core + platform-wayland 245 测试全绿（含 clip_route 12 用例 / ClipStore 公共 API 7 用例）；实机 smoke（mangowm）：`wl-copy` → data-control 事件 → 文本无损往返，不碰 IM seat 与运行中实例无竞争
- [x] 修复 M14 遗留：`context_seed_test` 的 `viterbi_sentences_seeded` 调用补 SpanCache 参数（main 上已编译失败，前轮「33 套件全绿」漏数了此文件）
- [ ] 二阶段：候选窗内搜索/删除单条历史、持久化开关（默认会话级）、primary selection 监听

## 版本口径（v0.49.33，2026-09-19）

**minor = 功能实数，patch = fix 提交累计**；major 恒 0（无 breaking）。
任务书：`.agent/tasks/versioning.md`（可执行操作手册：统计命令 / 合并规则 / 更新步骤 / 校验项，agent 直接照做）。

当前快照：**49 功能 / 33 fix**（98 提交闭环：38 feat 提交、33 fix、27 其他）。
为什么不是 silverq 的双 commit 计数（v0.18.16）：feat 提交粒度不稳定，
功能实数才是产品本体——minor 人审（发版数清单），patch 机数（git grep）。

功能清单（52 项，计数规则见剧本；新增追加、废弃移除）：

- [x] M1（4）：音节切分 404；词库 loader；精确+前缀查询；缩写兜底
- [x] M2（3）：双拼码表小鹤自然码；用户词自学习；自定义缩写映射
- [x] M3（1）：Wayland grab→preedit→commit 真上屏
- [x] M4（2）：wl_shm 渲染管线；cosmic-text CJK
- [x] M4.5（2）：input-popup 候选摆位；横排自适应宽度
- [x] M5（4）：翻页+数字选词；模糊音；音节补全；翻页键增强/Enter 原样/数字 0
- [x] M6（2）：192 万词条词库；范围查询 231x（跨过可用性边界）
- [x] M6.5（2）：内存双排序索引；二分查找热路径
- [x] M7（3）：FST build-dict；mmap -81% 内存；用户词 overlay
- [x] M8（4）：中文标点+顶字；TOML 配置；候选窗主题；Viterbi 句级联想
- [x] M9（4）：中英切换；托盘；LLM 联想；config 热更新
- [x] M10（2）：用户调频方案 B；分词混排
- [x] M11（1）：surrounding_text 上下文+末词种子
- [x] M12（2）：邻键纠错；半截键补全
- [x] M13（2）：eff 烘焙 7.5x；typing_probe 探针
- [x] M14（1）：SpanCache
- [x] M16（4）：剪贴板 ClipStore；deskctl snippets；`C-;` 交互路由；隐私投递路径
- [x] PR#27（1）：XIM server
- [x] PR#28（1）：中文优先排序（fix 提交，功能/fix 双计）
- [x] PR#32（4）：commit_log 采集；mine-lm 挖掘；bigram 上下文加成；自动组词

合计：4+3+1+2+2+4+2+2+3+4+4+2+1+2+2+1+4+1+1+4 = 49 ✓

## M17 — 待规划

- [ ] XIM 协议前端（解决 XWayland 光标对齐）
- [ ] 多模式输入（中英混合 + 自定义快捷短语）
- [ ] 云同步 / 词库同步
- [ ] 候选词优化续篇：比较器全路径统一 effective_freq（`place_sentences` / top_user）、PATHS_PER_NODE 增大下的碎切抑制、衰减函数长期稳定性
