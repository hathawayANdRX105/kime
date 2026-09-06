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

## M4 — 候选窗 UI（layer-shell）✅ 2026-09-06

- [x] layer-shell overlay 层、真实 wl_shm/memfd 渲染管线
- [x] cosmic-text 渲染 CJK 字体，实底深灰蓝底 + 白字
- [x] 真机像素扫描实锤：底部中央 87 行命中面板底色

## M4.5 — 候选窗光标跟随（input-popup-surface）✅ 2026-09-06

- [x] 协议正路改造：`im.get_input_popup_surface`，无 configure 握手
- [x] compositor 自动贴着光标定位；内容自适应高度
- [x] 真人实测：foot / QQ 贴着光标正常弹出；微信（XWayland）位置偏移记为已知边界

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
- [x] 候选窗主题样式与自适应紧凑尺寸（高对比度细边框，动态高度计算）
- [x] 句级 Word Lattice 构建与轻量 Viterbi 动态规划联想（第一候选输出长句）

## M9 — 进阶规划

- [ ] XIM 协议前端（解决部分老旧 XWayland 应用的光标对齐问题）
- [ ] 多模式输入（支持中英混合与自定义快捷短语管理）
