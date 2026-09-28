//! 有状态组合引擎 — 平台壳消费的唯一入口。
//!
//! 契约：壳把按键换算成 [`Key`] 喂 [`Engine::key`]，按 [`Outcome`] 分派：
//! - `Consumed` → 读 preedit / candidates / page 刷 UI
//! - `Ignored`  → 放行（wayland 下 release grab）
//! - `Commit`   → commit_string(文本)，engine 内部已清空组合状态
//!
//! 内部流：letters 累积 → [`kime_pinyin::segment`] → [`Dict::lookup_prefix`]
// → 候选。不做任何 I/O；切分非法时保持旧状态（无声可打即无候选）。

use crate::config::Config;
use crate::dict::{Candidate, Dict};
use crate::lattice::Seed;
use crate::punct;
use kime_pinyin::segment;

// evdev keycodes — wayland 原生即此值，平台壳无需翻译。
const KEY_ESC: u32 = 1;
const KEY_BACKSPACE: u32 = 14;
/// evdev KEY_DELETE（11）：组合内前删（#85）
const KEY_DELETE: u32 = 11;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_RIGHTSHIFT: u32 = 54;
const KEY_SPACE: u32 = 57;
const KEY_MINUS: u32 = 12;
const KEY_EQUAL: u32 = 13;
const KEY_LEFTBRACE: u32 = 26;
const KEY_RIGHTBRACE: u32 = 27;

/// 默认每页候选数（config.page_size = 0 时生效）
const DEFAULT_PAGE_SIZE: usize = 10;

/// 平台无关最小按键。壳负责从 wayland / TSF / IMKit 换算进来。
#[derive(Clone, Copy, Debug)]
pub struct Key {
    /// 可打印字符；功能键为 None（翻页/选词看 code）
    pub ch: Option<char>,
    /// evdev keycode（wayland 原生即此值）
    pub code: u32,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// kime 消费了此键
    Consumed,
    /// 不归 kime（英文模式等），壳放行
    Ignored,
    /// 上屏该文本
    Commit(String),
    /// 撤销选词（#79）：被弹条目消耗的拼音已还原进组合；壳须把这次物理退格
    /// 转发给应用，并再补发 `usize` 对合成退格点击——把该次选词上屏的字符
    /// 从应用文本里逐个删掉（物理退格删第 1 个，合成点击删其余）。
    UndoApp(usize),
    /// 智能标点撤销（#85，对齐 fcitx5 cancelLast）：空闲态退格撤销刚上屏的智能标点——
    /// 壳删除应用文本里的 `fullwidth`（已上屏全角字符）并上屏 `original`（原半角按键串）。
    /// 一次性：引擎发出后即清撤销状态，紧接着的第二次退格落 `Ignored`，不连环撤销。
    PuncCancel {
        /// 原半角按键串（如 "."）
        original: String,
        /// 已上屏的全角字符（如 "。"）
        fullwidth: String,
    },
}

pub struct Engine {
    dict: Dict,
    config: Config,
    chinese: bool,
    page_index: usize,
    /// 模糊音替换表（"zh" → "z" 等），Config::fuzzy 解析产物
    fuzzy_map: std::collections::HashMap<String, String>,
    /// 当前累积的拼音串（如 "niha"）
    letters: String,
    /// letters 内的光标位置（字符索引）。不变式：0..=letters.len()；
    /// letters 只由 a-z（ASCII）组成 → 字符索引==字节索引，直接当字节下标用是安全的。
    cursor: usize,
    /// 最近一次查询得到的候选；commit / clear 时一并清空。
    candidates: Vec<Candidate>,
    /// 双拼解码表（如小鹤/自然码），用于 shuangpin 模式
    sp: Option<kime_shuangpin::Table>,
    /// 当前候选是否出自双拼解码路径（含半截键 pending）。true 时选词消耗按**键位**计：
    /// 一个完整双拼音节恒等于 2 个键位，其拼音字母数与键位数无关（neng 4 字母但
    /// `ng` 只 2 键）——按字母消耗会吃掉下一音节的键位（长串逐字单选剩余拼音被截断的
    /// 根因，见 [`Self::select_candidate`]）。`refresh_candidates` 双拼解码成功分支置
    /// true（双拼回退全拼后仍无候选时也恢复 true：候选全来自双拼音节序列）；
    /// 全拼路径（双拼解码失败、或回退全拼查到了词）置 false。
    sp_active: bool,
    /// 首个切分的读音序列（preedit/光标的显示依据）；learn 仅在被选候选自带
    /// pinyin 缺失时回退到它
    last_reading: Vec<String>,
    /// 缓存的 preedit 字符串（双拼模式为解码后拼音，全拼为 letters）
    preedit: String,
    /// 当前标点模式（中文全角 / 英文原样）
    punct_mode: crate::config::PunctMode,
    /// 成对引号状态：`Some(q)` = 刚上屏过 `q` 的开引号，下一次再敲 `q` 出闭引号。
    /// 只覆盖 `"` 与 `'`（rime punctuator 同款最小状态机），其它键一按即重置。
    quote_open: Option<char>,
    /// 当前中文层一精确命中块的 joined key（音节 `'` 连接，与 `lookup_prefix` 同式）——
    /// `merge_english` 靠它认出「中文层一之后」的插入点。每次 refresh 刷新，清组合即清空。
    last_joined: String,
    /// 上一个键是否为数字（rime `digit_separators`：数字紧跟 ` , . : ` 时保持半角直通）。
    after_digit: bool,
    /// 光标前的输入框上下文（surrounding_text 归一化结果）。
    /// 上下文与组合生命周期无关：commit / Esc 清组合时**保留**它，
    /// 只由平台壳在 surrounding_text 事件到来时整体替换。
    context: Option<String>,
    /// 词图格子候选缓存（M14）：learn/import 失效，见 lattice::SpanCache。
    span_cache: crate::lattice::SpanCache,
    /// 上一次 kime 上屏的 (text, reading)，bigram 挖掘的「上文」。
    /// 比 surrounding_text 可靠：恒为 kime 自己的提交，不含粘贴/启动前的文本。
    last_commit: Option<(String, String)>,
    /// fcitx5 式词级撤销（#79 统一 undo）：最近若干次「选词」各自消耗的
    /// 拼音键串 + 上屏字符数（LIFO；全拼 = 消耗字母、双拼 = 消耗键位）。
    /// 生命周期 = 一条输入串（用户 09-28 裁定）：选词 push（cap 64），
    /// 只死在硬终结点——标点上屏 / Enter 原样 / Shift-flush / 切应用 / Esc。
    /// **全量选词上屏与 C-h 删空都不清链**（#76「面板关=链死」与 #77「删空
    /// 清栈」作废）：上屏的字必须能靠退格弹回（弹栈 + 壳补发应用退格删除）。
    /// 退格：栈非空 = 撤销最近选词（拼音还原 + UndoApp(n) 让壳删上屏字）；
    /// 栈净 = 逐字删拼音；双空 = 放行应用。
    undo_consumed: Vec<(String, usize)>,
    /// 预选缓冲（#81）：已选但**尚未释放**的词（LIFO；释放 = 拼音选完后的
    /// 空格 / 标点 / Enter / Shift-flush，整段一次性上屏）。上屏前退格
    /// = 弹最近选词（纯内部，应用零删除）；弹词时同序弹出该词在
    /// undo_consumed 里配对入栈的条目（未释放 = 无效撤销记录）。
    pending_words: Vec<String>,
    /// 缓存的 preedit **显示**串 = pending 拼接 + 剩余拼音（双拼为解码后
    /// 拼音、全拼为 letters）；壳层 set_preedit_string 用。
    preedit_display: String,
    /// 智能标点撤销状态（#85）：`Some((原半角按键串, 上屏全角字符))`。智能标点
    /// 上屏（中文模式 `map_punct` 命中转换，如 "."→"。"）后置位；下一键为
    /// 退格且空闲态（letters / pending / 候选全空）→ [`Outcome::PuncCancel`]，
    /// 发出即清。turn 化（对齐 fcitx5 lastIsPunc 的 3s 窗）：除退格外任何键
    /// 进 [`Self::key`] 入口即清除，过期态不会漏进删拼音 / 删词路径。
    last_punc: Option<(String, String)>,
}

impl Engine {
    /// 诊断/测试用：当前词图格子缓存跨度数。
    pub fn span_cache_len(&self) -> usize {
        self.span_cache.len()
    }

    /// 诊断/测试用：词图格子缓存是否为空。
    pub fn span_cache_is_empty(&self) -> bool {
        self.span_cache.is_empty()
    }

    /// 诊断/测试用：当前拼音字母串（KIME_DEBUG 状态日志用）。
    pub fn letters(&self) -> &str {
        &self.letters
    }

    /// 诊断/测试用：词级撤销栈深度（KIME_DEBUG 状态日志用）。
    pub fn undo_depth(&self) -> usize {
        self.undo_consumed.len()
    }

    /// 诊断/测试用：词级撤销栈顶（(消耗键串, 上屏字符数)，KIME_DEBUG 状态日志用）。
    pub fn undo_top(&self) -> Option<&(String, usize)> {
        self.undo_consumed.last()
    }

    /// 诊断/测试用：最近一次智能标点撤销状态（#85，(原半角按键串, 上屏全角字符)）。
    pub fn last_punc(&self) -> Option<&(String, String)> {
        self.last_punc.as_ref()
    }

    /// 清空词级撤销栈。
    ///
    /// 壳在「真实切应用 / IM 重连」（全新输入串开始）时调用：旧 burst 的
    /// 撤销历史不得跨应用边界存活——否则用户没输入任何键，退格就把旧词
    /// 弹进面板（v0.19.68 回归，投诉 2）。Esc / 同实例焦点抖动走
    /// [`Self::clear_composition`]（#77 起同样清栈，不再保留）。
    pub fn clear_undo(&mut self) {
        self.undo_consumed.clear();
    }

    /// 当前页大小：优先 config.page_size，否则 DEFAULT_PAGE_SIZE
    fn page_size(&self) -> usize {
        if self.config.page_size > 0 {
            self.config.page_size
        } else {
            DEFAULT_PAGE_SIZE
        }
    }

    pub fn new(dict: Dict, config: Config) -> Self {
        let punct_mode = config.punct_mode;
        let shuangpin = config.shuangpin;
        let mut fuzzy_map = std::collections::HashMap::new();
        for entry in &config.fuzzy {
            match entry.split_once('=') {
                Some((a, b)) if !a.is_empty() && !b.is_empty() => {
                    fuzzy_map.insert(a.to_string(), b.to_string());
                }
                _ => eprintln!("[kime] 忽略非法模糊音配置: {:?}", entry),
            }
        }
        Self {
            dict,
            config,
            chinese: true,
            letters: String::new(),
            cursor: 0,
            candidates: Vec::new(),
            sp: shuangpin.map(kime_shuangpin::Table::new),
            sp_active: false,
            last_reading: Vec::new(),
            preedit: String::new(),
            page_index: 0,
            fuzzy_map,
            undo_consumed: Vec::new(),
            pending_words: Vec::new(),
            preedit_display: String::new(),
            span_cache: crate::lattice::SpanCache::default(),
            punct_mode,
            quote_open: None,
            last_joined: String::new(),
            after_digit: false,
            context: None,
            last_commit: None,
            last_punc: None,
        }
    }
    /// 中/英文模式（英文模式所有键 Ignored 直通）
    pub fn chinese(&self) -> bool {
        self.chinese
    }

    /// 当前 preedit 显示串（#81：预选词拼接 + 未上屏拼音，如 "你好niha"）
    pub fn preedit(&self) -> &str {
        &self.preedit_display
    }

    /// (当前页, 页大小) — 候选窗布局用
    pub fn page(&self) -> (usize, usize) {
        (self.page_index, self.page_size())
    }

    /// 当前页首候选的全局索引（候选窗反色渲染用）
    pub fn highlight(&self) -> usize {
        self.page_index * self.page_size()
    }

    /// 当前读音的全部候选（内部 cap ~50，freq 降序）
    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }
    /// letters 空间的光标字符索引（平台无关，测试/诊断用）。
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// preedit 显示串内的光标字符索引 —— 壳报给 `set_preedit_string` 的 caret 位置。
    /// 全拼（含双拼解码失败回退全拼）时 preedit 恒等于 letters，逐字符 1:1；
    /// 双拼显示态两键对一个音节：光标前的完整键对贡献整音节长度，
    /// 奇数位光标补上半截键所代表的声母（与 refresh 的 pending 同一算法）。
    pub fn preedit_cursor(&self) -> usize {
        let pe_len = self.preedit.chars().count();
        // #81：显示串前缀 = 预选词拼接，caret 偏移同量
        let pending_len: usize = self.pending_words.iter().map(|w| w.chars().count()).sum();
        if self.sp.is_none() || self.preedit == self.letters {
            return pending_len + self.cursor.min(pe_len);
        }
        let full_pairs = self.cursor / 2;
        let mut idx: usize = self.last_reading[..full_pairs.min(self.last_reading.len())]
            .iter()
            .map(|s| s.chars().count())
            .sum();
        if self.cursor % 2 == 1 {
            if let Some(c) = self.letters.chars().nth(self.cursor - 1) {
                idx += self.sp.as_ref().unwrap().initial_of(c).chars().count();
            }
        }
        pending_len + idx.min(pe_len)
    }

    /// 更新输入框上下文尾巴（平台壳在 `surrounding_text` 事件时调用）。
    /// `None` = 光标前无可用上下文。与组合生命周期无关：清组合不清上下文。
    pub fn set_context(&mut self, tail: Option<String>) {
        self.context = tail;
    }

    /// 当前上下文尾巴（分词 / 排序的上下文输入），无上下文时 `None`。
    pub fn context_tail(&self) -> Option<&str> {
        self.context.as_deref()
    }
    /// 全量清空组合：候选/读音/preedit/页码/光标一并归位。所有上屏与 Esc 路径共用。
    ///
    /// #79 起清组合 = 硬终结点：撤销链随之清空（Esc / 壳层焦点抖动的合成
    fn clear_composition(&mut self) {
        self.letters.clear();
        self.candidates.clear();
        self.last_reading.clear();
        self.preedit.clear();
        self.page_index = 0;
        self.cursor = 0;
        self.last_joined.clear();
        self.sp_active = false;
        self.undo_consumed.clear();
        self.pending_words.clear();
        self.preedit_display.clear();
    }

    /// 重建 preedit 显示串 = 预选词拼接 + 剩余拼音缓存（#81）。
    /// 所有改 pending / preedit 缓存的路径末尾调一次。
    fn rebuild_preedit_display(&mut self) {
        let mut s: String = self.pending_words.concat();
        s.push_str(&self.preedit);
        self.preedit_display = s;
    }

    /// 选词入 pending（#81）：learn + 清空候选/页码，但**只消耗被选候选
    /// 覆盖的音节**，剩余字母重组回 letters 继续组词；词进
    /// pending_words 缓冲（不立即上屏，等释放：拼音选完后的空格 /
    /// 标点 / Enter / Shift-flush 整段上屏）。撤销栈条目同步入栈
    /// （未释放前无效：退格弹 pending 时成对弹出；释放后存活为
    /// #80 LIFO 条目）。
    ///
    /// 候选自带读音（pinyin 按 `'` 切分）：打长串 `womendoubuzhidao`
    /// 翻页选了「我」后剩余音节拼回字母串，refresh_candidates 重新
    /// 出候选，可继续选「们」「都」。双拼按**键位**消耗（每音节恒
    /// 2 键），按字母会吃掉下一音节键位。候选无自带读音（AI 注入）
    /// 时回退整串消耗。
    fn select_candidate(&mut self, cand: &Candidate) -> Outcome {
        let text = cand.text.clone();
        self.learn_or_warn(cand);

        let consumed: Vec<String> = if cand.pinyin.is_empty() {
            self.last_reading.clone()
        } else {
            cand.pinyin.split('\'').map(str::to_string).collect()
        };
        // 剩余键串 = 原串去掉被选候选覆盖的音节。全拼按字母长度消耗；
        // 双拼按键位消耗（见 doc）。
        let old_len = self.letters.len();
        let remaining = if self.sp_active {
            let k = consumed.len();
            let cut = 2 * k;
            if cut >= self.letters.len() {
                String::new()
            } else {
                self.letters[cut..].to_string()
            }
        } else {
            consume_prefix_letters(&self.letters, &consumed)
        };

        self.candidates.clear();
        self.page_index = 0;
        self.last_joined.clear();
        self.preedit.clear();
        self.last_reading.clear();
        // 词级撤销：记下本次选词消耗的拼音键串（原串前段）+ 上屏字符数。
        let undo_entry = self.letters[..old_len - remaining.len()].to_string();
        self.letters = remaining;
        if !undo_entry.is_empty() {
            self.undo_consumed.push((undo_entry, text.chars().count()));
            if self.undo_consumed.len() > 64 {
                self.undo_consumed.remove(0);
            }
        }
        // 词进 pending 缓冲（#81：不立即上屏）
        self.pending_words.push(text);
        // 光标落在剩余拼音末尾：caret 在「最后」，后续打字续在尾部。
        self.cursor = self.letters.len();

        if !self.letters.is_empty() {
            self.refresh_candidates();
        }
        self.rebuild_preedit_display();
        Outcome::Consumed
    }

    /// 唯一入口。字母累积（光标处插入）/ 退格删光标前字符 / 组合内光标编辑（C-b/C-f/C-h）
    /// / 数字选词 / 空格首选 / shift 中英切换
    pub fn key(&mut self, k: Key) -> Outcome {
        // #85 标点撤销 turn 化：除退格外任何键进入口即清 last_punc（字母入组合 /
        // 数字选词 / 空格释放 / 再标点 / Esc / Shift-flush / 翻页…全覆盖），
        // 过期撤销态不会漏进后续删除路径。
        if k.code != KEY_BACKSPACE {
            self.last_punc = None;
        }
        // 数字标志：数字键无论 outcome（选词命中/放行）都置位，其余任何键清零。
        let after_digit = std::mem::replace(
            &mut self.after_digit,
            k.ch.is_some_and(|c| c.is_ascii_digit()),
        );
        // 成对标点状态机入口：同一个引号键连按交替 开→闭→开；任何其它键（字母、
        // 空格、ESC、英文模式下的任何东西）都重置回「下一次出开引号」。
        let quote_key = match k.ch {
            Some(c @ ('"' | '\'')) => Some(c),
            _ => None,
        };
        if quote_key != self.quote_open {
            self.quote_open = None;
        }
        // Shift 单独按下（#83 起无持久模式）：无组合 = 空操作（轻点锁死根子
        // 已删：英文字母靠「按住 Shift 打字」临时透传表达，壳层手势裁决层
        // 也不再补交切模式）；有组合 = 上屏原字母串（输入串结束，撤销链
        // 随组合消亡），模式不变。
        if k.ch.is_none()
            && k.shift
            && !k.ctrl
            && !k.alt
            && (k.code == KEY_LEFTSHIFT || k.code == KEY_RIGHTSHIFT)
        {
            // #81：Shift-flush 连同 pending 整段冲刷（硬终结点：清组合含
            // 预选词与撤销栈，输入串结束）
            let pending_text = self.pending_words.join("");
            if self.letters.is_empty() && pending_text.is_empty() {
                return Outcome::Consumed;
            }
            let text = format!("{pending_text}{}", self.letters);
            // 清组合（#77 起同步清撤销栈）：Shift-flush 原样上屏 = 输入串结束。
            self.clear_composition();
            return Outcome::Commit(text);
        }

        // 英文模式（#83 起休眠：chinese 恒 true，本分支不可达；保留待后续
        // 模式系统清理）：其余键一律放行。
        if !self.chinese {
            self.quote_open = None;
            return Outcome::Ignored;
        }

        // Backspace — evdev KEY_BACKSPACE（14），ch 通常为 None。#77 撤销优先
        // （fcitx5 useBackSpaceToUnselect 对齐）：撤销栈非空（无论 letters 空否）
        // → 弹栈顶还原最近选词消耗的拼音（letters = 消耗段 + 剩余，光标归尾，
        // 候选重建，可再选词、也可继续组词）；栈净后才删光标**前**一个字符
        // （组合内光标语义），删空即清栈（#77 不变式）；双空 = 空闲态：#85
        // 标点撤销（last_punc 在 → PuncCancel，一次性）否则 Ignored 放行给应用。
        // 长按的自动重复按 press 逐次到达，与引擎无状态假设一致。
        if k.ch.is_none() && k.code == KEY_BACKSPACE {
            // #81 pending 弹词最优先：释放前退格 = 弹最近预选词（纯内部，
            // 应用零删除），并成对弹出该词在 undo_consumed 里的无效条目；
            // pending 弹空后才进 #80 释放后 LIFO（undo_consumed / UndoApp）。
            // 非空闲退格是消费键：清掉过期标点撤销态（#85）；双空（空闲态）
            // 不在此清，留给下面的 PuncCancel 消费。
            if !self.pending_words.is_empty() {
                self.last_punc = None;
                self.pending_words.pop();
                // 该词消耗的拼音弹回组合（可重新选词/组词），成对弹出
                // undo_consumed 里该词的无效条目
                let (consumed, _chars) = self.undo_consumed.pop().unwrap_or_default();
                let remaining = std::mem::take(&mut self.letters);
                self.letters = format!("{consumed}{remaining}");
                self.cursor = self.letters.len();
                self.page_index = 0;
                self.refresh_candidates();
                self.rebuild_preedit_display();
                return Outcome::Consumed;
            }
            // 撤销优先（#77 起）+ #79 统一 undo：栈非空（无论 letters 空否）
            // = 撤销最近选词：拼音还原进组合（消耗段 + 剩余）
            if let Some((consumed, app_chars)) = self.undo_consumed.pop() {
                self.last_punc = None;
                let remaining = std::mem::take(&mut self.letters);
                self.letters = format!("{consumed}{remaining}");
                self.cursor = self.letters.len();
                self.page_index = 0;
                self.refresh_candidates();
                return Outcome::UndoApp(app_chars.saturating_sub(1));
            }
            if !self.letters.is_empty() {
                self.last_punc = None;
                if self.cursor > 0 {
                    self.letters.remove(self.cursor - 1);
                    self.cursor -= 1;
                }
                self.refresh_candidates();
                self.page_index = 0;
                return Outcome::Consumed;
            }
            // 双空 = 空闲态：#85 智能标点撤销（对齐 fcitx5 cancelLast，一次性）——
            // 最近一次智能标点上屏记录在 last_punc → 发 PuncCancel（壳删全角
            // 上屏 original），发出即清；无记录 → Ignored 放行给应用删字符。
            if let Some((original, fullwidth)) = self.last_punc.take() {
                return Outcome::PuncCancel {
                    original,
                    fullwidth,
                };
            }
        }

        // Esc — 有组合时清空并消费；无组合时放行给应用（退出全屏 vim、
        // 关闭对话框等）。之前无条件 Consumed，空组合按 ESC 被吞掉，
        // 与英文模式行为不一致。
        if k.ch.is_none() && k.code == KEY_ESC {
            if self.letters.is_empty() && self.pending_words.is_empty() {
                return Outcome::Ignored;
            }
            // #81：Esc = 整句取消——预选词 + 剩余拼音全清，不上屏；
            // 已释放词的撤销链（undo_consumed）不动。clear_composition
            // 同时清 pending 与显示串。
            self.clear_composition();
            return Outcome::Consumed;
        }

        // 翻页：
        //   功能码路径：- / [ 上一页，= / ] 下一页（ch 为 None 时走这里）
        //   字符路径：- 上一页，+ / = 下一页（ch 非 None 时走这里）
        if k.ch.is_none()
            && matches!(
                k.code,
                KEY_MINUS | KEY_EQUAL | KEY_LEFTBRACE | KEY_RIGHTBRACE
            )
        {
            if self.candidates.is_empty() {
                return Outcome::Ignored;
            }
            let total_pages = self.candidates.len().div_ceil(self.page_size());
            match k.code {
                KEY_MINUS | KEY_LEFTBRACE => {
                    self.page_index = self.page_index.saturating_sub(1);
                }
                _ => {
                    self.page_index = (self.page_index + 1).min(total_pages - 1);
                }
            }
            return Outcome::Consumed;
        }
        // 字符路径翻页（+ / = 下一页，- 上一页）—— 在中文模式下拦截这些键
        if !self.candidates.is_empty() {
            if let Some('+') | Some('=') = k.ch {
                if !k.ctrl && !k.alt {
                    let total_pages = self.candidates.len().div_ceil(self.page_size());
                    self.page_index = (self.page_index + 1).min(total_pages - 1);
                    return Outcome::Consumed;
                }
            }
            if let Some('-') = k.ch {
                if !k.ctrl && !k.alt && !k.shift {
                    self.page_index = self.page_index.saturating_sub(1);
                    return Outcome::Consumed;
                }
            }
        }

        // 组合内光标编辑（用户要求）：Ctrl+B 左移一格、Ctrl+F 右移一格、Ctrl+H 删光标前
        // 一个字符。**Ctrl+F/B 不再翻页**——翻页让位给 - / = 与 Ctrl+N/Ctrl+P（下方），
        // 这是用户的明确取舍（「C-f 前进一个字符、C-b 后退一个字符」）。
        // 只在有组合时拦截：空组合下 Ctrl+F/B/H 一律 Ignored，应用的 emacs 移动键照旧可用。
        if k.ctrl && !k.alt && !self.letters.is_empty() {
            match k.ch {
                Some('b') => {
                    self.cursor = self.cursor.saturating_sub(1);
                    return Outcome::Consumed;
                }
                Some('f') => {
                    self.cursor = (self.cursor + 1).min(self.letters.len());
                    return Outcome::Consumed;
                }
                Some('h') => {
                    // #79：删空不清撤销链（#77 不变式作废）——部分选词后 C-h
                    // 删净剩余拼音，退格仍可逐条弹上屏字，回退统一。
                    if self.cursor > 0 {
                        self.letters.remove(self.cursor - 1);
                        self.cursor -= 1;
                        self.refresh_candidates();
                        self.page_index = 0;
                    }
                    return Outcome::Consumed;
                }
                _ => {}
            }
        }
        // #85 Delete 前删：组合内（letters 有拼音字符）按 KEY_DELETE（11）删光标
        // 后 1 个拼音字符（对照 C-h 的后删，方向反转）；组合空 → Ignored 放行
        // 应用前删。pending 预选词是已选词不是拼音，不参与前删。
        if k.ch.is_none() && k.code == KEY_DELETE && !self.letters.is_empty() {
            if self.cursor < self.letters.len() {
                self.letters.remove(self.cursor);
                self.refresh_candidates();
                self.page_index = 0;
            }
            return Outcome::Consumed;
        }
        // Ctrl+n / Ctrl+p 翻页（Emacs 风格，无候选 Ignored，越界钳位）
        if k.ctrl && !k.alt && !k.shift && !self.candidates.is_empty() {
            let total_pages = self.candidates.len().div_ceil(self.page_size());
            match k.ch {
                Some('n') => {
                    self.page_index = (self.page_index + 1).min(total_pages - 1);
                    return Outcome::Consumed;
                }
                Some('p') => {
                    self.page_index = self.page_index.saturating_sub(1);
                    return Outcome::Consumed;
                }
                _ => {}
            }
        }

        // 空格 — 有候选 = 选当前页首候选入 pending（#81：不再写死第一页，
        // 翻页 page_index 参与提交）；无候选且有 pending = 整段释放上屏；
        // 双空 = 放行。
        if k.ch.is_none() && k.code == KEY_SPACE {
            if !self.candidates.is_empty() {
                let top = self.candidates[self.highlight()].clone();
                return self.select_candidate(&top);
            }
            if !self.pending_words.is_empty() {
                // 整段释放（#81）：pending 一次性上屏。撤销链**不清**——
                // 释放后退格走 #80 LIFO 逐词弹回 + 应用级删除。
                let text = self.pending_words.join("");
                self.pending_words.clear();
                self.preedit_display.clear();
                return Outcome::Commit(text);
            }
            return Outcome::Ignored;
        }
        // Ctrl+. 切换标点模式（中文全角 ↔ 英文原样）
        if k.ctrl && !k.alt && k.ch == Some('.') {
            self.punct_mode = match self.punct_mode {
                crate::config::PunctMode::Chinese => crate::config::PunctMode::English,
                crate::config::PunctMode::English => crate::config::PunctMode::Chinese,
            };
            return Outcome::Consumed;
        }
        // 标点处理（中文模式）：全表对齐 rime half_shape（32 条，parity 测试钉死）；
        // digit_sep/ctrl/alt 组合不入映射，落到底部 Ignored 直通。
        if let Some(c) = k.ch {
            let digit_sep = after_digit && matches!(c, ',' | '.' | ':');
            if let Some(mapped) = punct::map_punct(c).filter(|_| !digit_sep && !k.ctrl && !k.alt) {
                // 英文标点模式：不转换，原样输出（也不维护引号状态）
                if self.punct_mode == crate::config::PunctMode::English {
                    self.quote_open = None;
                    // 标点上屏 = 输入串结束：清空撤销栈
                    self.undo_consumed.clear();
                    return Outcome::Commit(c.to_string());
                }
                // 成对引号：map_punct 只给开引号；连着按的第二次在这里换成闭引号。
                let closing = self.quote_open == Some(c);
                if quote_key.is_some() {
                    self.quote_open = if closing { None } else { Some(c) };
                }
                let mapped: &str = match (c, closing) {
                    ('"', true) => "”",
                    ('\'', true) => "’",
                    _ => mapped,
                };
                // #85 标点撤销：实际转换（上屏 "。" 等全角字符）时置撤销状态——
                // 空闲态退格发 PuncCancel，壳删 `mapped` 上屏原 `c`。自映射
                // 条目（mapped == c，无转换）不置。
                let converted = *mapped != c.to_string();
                if self.letters.is_empty() {
                    // 情况 A：无预编辑串。有 pending = 释放 pending + 标点；
                    // 无 pending = 直接上屏标点（#77 不变式：letters 空 ⟹ 链空）。
                    if self.pending_words.is_empty() {
                        if converted {
                            self.last_punc = Some((c.to_string(), mapped.to_string()));
                        }
                        return Outcome::Commit(mapped.to_string());
                    }
                    let text = self.pending_words.join("");
                    self.pending_words.clear();
                    self.preedit_display.clear();
                    // 标点上屏 = 输入串结束：清空撤销栈
                    self.undo_consumed.clear();
                    if converted {
                        self.last_punc = Some((c.to_string(), mapped.to_string()));
                    }
                    return Outcome::Commit(format!("{text}{mapped}"));
                } else if !self.candidates.is_empty() {
                    // 情况 B（#81）：选当前页首候选入 pending，整段释放
                    // pending + 标点；标点 = 输入串结束（硬终结点清栈）。
                    let top = self.candidates[self.highlight()].clone();
                    self.select_candidate(&top);
                    let text = self.pending_words.join("");
                    self.pending_words.clear();
                    self.preedit_display.clear();
                    self.undo_consumed.clear();
                    if converted {
                        self.last_punc = Some((c.to_string(), mapped.to_string()));
                    }
                    return Outcome::Commit(format!("{text}{mapped}"));
                } else {
                    // 情况 C：无候选词，字母串（连同 pending 整段）+ 标点
                    // 原样上屏（= 输入串结束：clear_composition 清 pending 与撤销栈）
                    let pending_text = self.pending_words.join("");
                    let text = format!("{pending_text}{}", self.letters);
                    self.clear_composition();
                    if converted {
                        self.last_punc = Some((c.to_string(), mapped.to_string()));
                    }
                    return Outcome::Commit(format!("{text}{mapped}"));
                }
            }
        }
        if let Some(c) = k.ch {
            if c.is_ascii_alphabetic() {
                if k.alt || k.ctrl {
                    return Outcome::Ignored;
                }
                let lc = c.to_ascii_lowercase();
                self.letters.insert(self.cursor, lc);
                self.cursor += 1;
                self.refresh_candidates();
                return Outcome::Consumed;
            }
            // 数字 0-9：当前页内选词（0 选第 10 个，1-9 选第 1-9 个）
            if let Some(d) = c.to_digit(10) {
                let digit = d as usize;
                if digit == 0 || (1..=9).contains(&digit) {
                    let offset = if digit == 0 { 9 } else { digit - 1 };
                    let idx = self.page_index * self.page_size() + offset;
                    if let Some(cand) = self.candidates.get(idx).cloned() {
                        let r = self.select_candidate(&cand);
                        // 数字在此被消费成中文候选上屏，不是输出字面数字；
                        // after_digit 必须清零，否则下一个 ,.: 会被误当数字分隔符放行半角。
                        self.after_digit = false;
                        return r;
                    }
                }
            }
        }
        // Enter（code 28）— 上屏原始字母串、面板退出，不提交候选词：
        //   有字母 → 原样上屏（声明「我打的是字母不是拼音」），组合清空、
        //            面板消失、撤销栈清空（输入串结束）。不转发回车键本身——QQ/微信
        //            收到回车会把消息直接发出去；要换行由应用自己的后续按键决定。
        //   无字母 → Ignored，回车原样放行。
        if k.ch.is_none() && k.code == 28 {
            if !self.letters.is_empty() {
                // 有字母（#81：连同 pending 整段）原样上屏，组合清空、面板
                // 消失、撤销栈清空（输入串结束）。
                let text = format!("{}{}", self.pending_words.join(""), self.letters);
                self.clear_composition();
                return Outcome::Commit(text);
            }
            if !self.pending_words.is_empty() {
                // 无字母有 pending（#81）：Enter = 整段释放，撤销链存活
                // （释放后 LIFO 可退格弹回）
                let text = self.pending_words.join("");
                self.pending_words.clear();
                self.preedit_display.clear();
                return Outcome::Commit(text);
            }
            return Outcome::Ignored;
        }

        // 其它（标点、功能键等）→ 放行给宿主。
        Outcome::Ignored
    }
}

/// 从字母串头部消费掉给定音节，返回剩余字母。
///
/// 音节按全拼规则连续占据字母串头部（`wo` + `men` + ...）。逐音节匹配：
/// 大小写不敏感，匹配上就推进；首个音节匹配失败（候选读音与当前组合
/// 不同源，如多切分/模糊音变体）时回退——按音节总长度截断，对齐不上
/// 就整串消费（返回空，保持旧行为）。
fn consume_prefix_letters(letters: &str, syllables: &[String]) -> String {
    if syllables.is_empty() {
        return letters.to_string();
    }
    let mut consumed_len = 0usize;
    let mut rest = letters;
    for syl in syllables {
        let syl_lower = syl.to_ascii_lowercase();
        if let Some(stripped) = rest.strip_prefix(&syl_lower) {
            consumed_len += syl.len();
            rest = stripped;
        } else if let Some(stripped) = rest.strip_prefix(&syl.to_ascii_uppercase()) {
            consumed_len += syl.len();
            rest = stripped;
        } else {
            // ponytail: 对齐失败时按音节总长度截断；长度越界则整串消费。
            let total: usize = syllables.iter().map(|s| s.len()).sum();
            if total >= letters.len() {
                return String::new();
            }
            return letters[total..].to_string();
        }
    }
    letters[consumed_len..].to_string()
}

/// 对一个音节/尾部串应用模糊替换表：前缀匹配（声母）与后缀匹配（韵母）各生成一路变体。
fn fuzzy_expand(map: &std::collections::HashMap<String, String>, s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (from, to) in map {
        if s.starts_with(from.as_str()) && !s.starts_with(to.as_str()) {
            out.push(format!("{}{}", to, &s[from.len()..]));
        }
        if s.ends_with(from.as_str()) && !s.ends_with(to.as_str()) {
            out.push(format!("{}{}", &s[..s.len() - from.len()], to));
        }
    }
    out
}

/// 与 `Dict::lookup_prefix` 完全同式的 key 拼接：音节 `'` 连接，尾部半截音节续在最后。
/// 整句落位要靠它识别「候选/句子是否恰好消耗完当前输入」。
fn joined_key(syllables: &[String], tail: &str) -> String {
    let mut joined = syllables.join("'");
    if !tail.is_empty() {
        if !joined.is_empty() {
            joined.push('\'');
        }
        joined.push_str(tail);
    }
    joined
}

impl Engine {
    /// 学习以**被选候选自己的 pinyin** 为准：多切分查询后 last_reading 只反映首切分
    /// （打 "dangao" 选「蛋糕」时它是 ["dang"]），拿它学习会把用户词学到错误读音下。
    /// 候选自带读音 = pinyin 按 `'` 切分；外部注入（AI）候选没带时回退 last_reading。
    fn learn_or_warn(&mut self, cand: &Candidate) {
        let reading: Vec<String> = if cand.pinyin.is_empty() {
            self.last_reading.clone()
        } else {
            cand.pinyin.split('\'').map(str::to_string).collect()
        };
        // 离线 LM 原材料：记录 (上次提交词, 本词) 供挖掘 bigram。
        // 失败不阻塞上屏（log_commit 内部已吞错）。
        let prev = self.last_commit.take();
        self.dict.log_commit(
            prev.as_ref().map(|(t, r)| (t.as_str(), r.as_str())),
            &reading,
            &cand.text,
        );
        self.last_commit = Some((cand.text.clone(), reading.join("'")));
        // LM 上下文 = 刚提交的词：装载其后继计数，下一次按键的候选排序即生效。
        // 顺手检查世代号（离线挖掘跑过则重载缓存）。
        self.dict.set_lm_context(
            self.last_commit
                .as_ref()
                .map(|(t, r)| (t.as_str(), r.as_str())),
        );
        if let Err(e) = self.dict.learn(&reading, &cand.text) {
            eprintln!(
                "[kime] 用户词学习失败 ({} → {}): {}",
                self.preedit, cand.text, e
            );
        }
        // learn 改了条目 eff → 词图格子缓存整体作废（失效点：凡词库内容/eff 变化）
        self.span_cache.clear();
    }

    /// 候选查询：双拼模式走 Table::to_syllables 解码，全拼模式走 kime_pinyin::segment + lookup_prefix，
    /// 兜底 lookup_abbrev。维护 self.last_reading 与 self.preedit。
    ///
    /// 平台壳在上下文变化（input-method-v2 的 `done` 提交 surrounding_text）后调一次：
    /// 壳写完 `set_context` 立刻调它，候选即按新上下文重排，不必等下一次按键。
    /// 按键路径内部在组合变化时自动调，壳层请勿在按键路径里重复调。
    pub fn refresh_candidates(&mut self) {
        if self.letters.is_empty() {
            self.candidates.clear();
            self.last_reading.clear();
            self.preedit.clear();
            self.rebuild_preedit_display();
            self.last_joined.clear();
            return;
        }

        // 双拼模式：解码失败时回退全拼切分（允许全拼混输，与 fcitx5 双拼行为一致）
        if self.sp.is_some() {
            let len = self.letters.len();
            // 解码和半截键的拼音前缀都在块表达式里算完，`self.sp` 的借用随块结束：
            // 后面要 `&mut self` 写 candidates / 调 refresh_full_pinyin。
            let (decoded, pending, half_key_syls) = {
                let table = self.sp.as_ref().unwrap();
                if len.is_multiple_of(2) {
                    match table.to_syllables(&self.letters) {
                        ok @ Ok(_) => (ok, String::new(), Vec::new()),
                        Err(_) => {
                            // 非法键对（如自然码 xk=x+ao「xao」不存在的音节）：
                            // 双拼键序回退全拼几乎必然无解——键序含 v 等全拼
                            // 不存在的字母，segment 必空 → 0 候选 → 候选窗
                            // 消失、翻页失效。改为解码最长合法偶数前缀（剩余
                            // 键不参与查询），保住已敲部分的候选；连首键对都
                            // 解不出时保持 Err，由下游 match 退全拼混输兜底。
                            let mut fb: Result<Vec<String>, String> = Err(String::new());
                            for cut in (2..len).step_by(2).rev() {
                                if let ok @ Ok(_) = table.to_syllables(&self.letters[..cut]) {
                                    fb = ok;
                                    break;
                                }
                            }
                            (fb, String::new(), Vec::new())
                        }
                    }
                } else {
                    // 最后一个键还没凑成键对，它代表的是**声母**而不是拼音字母：
                    // 直接拿它当拼音前缀去查，等于查 "u" 开头的词，半截状态必出垃圾。
                    let last = self.letters[len - 1..].chars().next();
                    let last = last.unwrap_or(' ');
                    (
                        table.to_syllables(&self.letters[..len - 1]),
                        table.initial_of(last),
                        // 公共前缀塌缩为空（y/w/元音零声母键）时，前缀收窄不了
                        // 任何东西；留下完整音节集给补全路径（见下）。
                        if table.initial_of(last).is_empty() {
                            table.syllables_of_initial(last)
                        } else {
                            Vec::new()
                        },
                    )
                }
            };
            match decoded {
                Ok(syllables) => {
                    let has_pending = !pending.is_empty();
                    let pending = pending.as_str();
                    let sp_joined = joined_key(&syllables, pending);
                    self.sp_active = true;
                    self.last_reading = syllables.clone();
                    self.preedit = format!("{}{}", syllables.join(""), pending);
                    self.rebuild_preedit_display();
                    self.last_joined = sp_joined.clone();
                    let mut cands = self
                        .dict
                        .lookup_prefix(&syllables, pending, self.config.candidate_limit)
                        .unwrap_or_default();
                    // 半截键补全（零声母键 y/w/元音：公共前缀为空，前缀查询退化成
                    // 「首音节全量」）：枚举半截键能拼出的完整音节，逐个作为已完成
                    // 的末音节补查。用户敲半截键的意图是「末音节以这个字母开头」，
                    // 末音节完整的词（ke+y → ke'yi「可以」）比任意 ke* 词更贴合意图
                    // ——补全命中排在前，其余裸前缀结果去重后续后。
                    if !half_key_syls.is_empty() {
                        let mut seen: std::collections::HashSet<String> =
                            cands.iter().map(|c| c.text.clone()).collect();
                        let mut completions: Vec<Candidate> = Vec::new();
                        for s in &half_key_syls {
                            let mut r = syllables.clone();
                            r.push((*s).to_string());
                            if let Ok(mut vc) =
                                self.dict.lookup_prefix(&r, "", self.config.candidate_limit)
                            {
                                vc.retain(|c| seen.insert(c.text.clone()));
                                completions.extend(vc);
                            }
                            if completions.len() >= self.config.candidate_limit {
                                break;
                            }
                        }
                        // completions + 原前缀结果，总长截到 candidate_limit
                        let mut merged = completions;
                        let keep = self.config.candidate_limit.saturating_sub(merged.len());
                        merged.extend(cands.into_iter().take(keep));
                        cands = merged;
                    }
                    self.candidates = cands;
                    if self.candidates.is_empty() {
                        if let Ok(ab) = self
                            .dict
                            .lookup_abbrev(&self.letters, self.config.candidate_limit)
                        {
                            self.candidates = ab;
                        }
                    }
                    if self.candidates.is_empty() {
                        // 双拼解出错误音节（如 nihao→ni+ha）且查无词 → 全拼重试。
                        // 先试全拼：混输的键串按全拼才是对的，此时不该拿双拼音节硬凑句子。
                        self.refresh_full_pinyin();
                        if self.candidates.is_empty() {
                            // 全拼也查无词：键串仍只能按双拼键位解释（下面的整句
                            // 联想 / 缺陷 A 回退候选全部来自 syllables），消耗必须
                            // 回到 2 键/音节，否则整句场景选单字又会按拼音字母数
                            // 多吃下一音节的键位。
                            self.sp_active = true;
                        } else {
                            // 全拼解释成立（preedit / last_reading / 候选全是全拼
                            // 字母语义）：到此为止。下面的整句联想与首音节单字都由
                            // syllables 生成，混进来会让同一列表混两套消耗语义——
                            // 选这类双拼单字时 sp_active 已是 false，按字母切会切错
                            // 键位：`aang` 解 a+neng 查无词、全拼命中「阿昂」后若再
                            // 混进「啊」，选它剩 `ang`，正确是 `ng` 即 neng。
                            self.merge_english();
                            return;
                        }
                    }
                    // 整句联想：词库没有整串词条时（「我不知道你说的是什么」这类长句），
                    // Viterbi 组词是唯一的候选来源。双拼分支此前完全没接，长句一律 0 候选。
                    if syllables.len() >= 2 {
                        let seed = self.context_seed();
                        let sentences = crate::lattice::viterbi_sentences_seeded(
                            &self.dict,
                            &syllables,
                            seed,
                            &mut self.span_cache,
                        );
                        if !sentences.is_empty() && self.candidates.is_empty() {
                            // 全拼重试也没结果 → 双拼解读才是对的，恢复它的 preedit/读音
                            self.last_reading = syllables.clone();
                            self.preedit = format!("{}{}", syllables.join(""), pending);
                            self.rebuild_preedit_display();
                            self.last_joined = sp_joined.clone();
                        }
                        // pending（半截键对）非空时句子没有消耗全部输入 → 不抢层一名次。
                        Self::place_sentences(&mut self.candidates, sentences, &sp_joined);
                    }
                    // 缺陷 A 回退：以上全部落空（整串无词条、Viterbi 拼不出全覆盖句）
                    // 时，砍末音节逐档重查，让候选列表非空——用户至少能选到首音节的字。
                    // preedit / last_reading / last_joined 仍是整串，回退只补候选。
                    if self.candidates.is_empty() {
                        if let Some(fb) = self.longest_prefix_candidates(&syllables) {
                            self.candidates = fb;
                        }
                    }
                    // 长串降级（与全拼路径同约）：整句/长组合词占满但不足一页时，
                    // 首音节单字候选追加到末尾，翻页翻得到「我」。全拼重试已经跑过
                    // 这条路径时结果已在列表里，按文本去重后是空操作。
                    // 双拼降级：整句/组合词占满但不足一页时追加首音节单字。
                    // 双拼一个键对就是一个音节，≥2 音节即可能是「漏了第二个拼音」
                    // （nimf 想打 ni 但被解成 nimen）——用户要的是先选「你」再继续，
                    // 不是被锁在「你们」整句里。末键半截（has_pending）时同样强制。
                    // 此分支只在双拼模式内，全拼的 context_seed 契约不受影响。
                    if !self.candidates.is_empty() && (has_pending || syllables.len() >= 2) {
                        Self::append_first_syllable_candidates(
                            &mut self.candidates,
                            &self.dict,
                            &syllables,
                            self.config.candidate_limit,
                            true,
                        );
                    }
                }
                Err(_) => {
                    // 非法键对：按全拼重新切分（preedit 保持原字母串）
                    self.refresh_full_pinyin();
                }
            }
            self.merge_english();
            return;
        }

        self.refresh_full_pinyin();
        self.merge_english();
    }

    /// 英文词候选：rime 的英文走独立 translator，匹配**原始按键串**、不做拼音解码，
    /// 所以双拼/全拼下行为一致。两个字母起才查，免得单字母把 `a/AA/an` 之类的英文噪声
    /// 灌进每一次按键。
    ///
    /// 落位（用户裁定）：**中文候选全部在前**——英文块（精确词 + 补全词）整体追加到
    /// 列表末尾，不再按层一 splice。`lookup_english` 内部恒「精确在前、补全在后」，
    /// 英文块内顺序不变；层一「精确命中优先于补全」的中文内部规则完全不受影响。
    fn merge_english(&mut self) {
        if self.letters.len() < 2 {
            return;
        }
        let en = self
            .dict
            .lookup_english(&self.letters, self.config.candidate_limit);
        if en.is_empty() {
            return;
        }
        self.candidates.extend(en);
    }

    /// 全拼路径：segment 全部切分逐条前缀查询 + 模糊音 + abbrev 兜底 + Viterbi 句级联想。
    fn refresh_full_pinyin(&mut self) {
        // 全拼候选按拼音字母消耗键串（双拼解码失败回退全拼时也走这里）。
        self.sp_active = false;
        let segs = segment(&self.letters);
        let (reading, tail): (Vec<String>, String) = if let Some(reading) = segs.first().cloned() {
            let mut r = reading;
            let tail = r.pop().unwrap_or_default();
            (r, tail)
        } else {
            let mut found: Option<(Vec<String>, String)> = None;
            for i in (1..self.letters.len()).rev() {
                let head_segs = segment(&self.letters[..i]);
                if let Some(reading) = head_segs.into_iter().next() {
                    found = Some((reading, self.letters[i..].to_string()));
                    break;
                }
            }
            match found {
                Some((r, t)) => (r, t),
                None => (Vec::new(), self.letters.clone()),
            }
        };

        self.last_reading = reading.clone();
        self.preedit = self.letters.clone();
        self.rebuild_preedit_display();
        self.last_joined = joined_key(&reading, &tail);

        // 主路径查询（首切分，显示与层一契约以它为准）
        let mut cands: Vec<Candidate> = self
            .dict
            .lookup_prefix(&reading, &tail, self.config.candidate_limit)
            .unwrap_or_default();

        // 模糊音变体：对完整音节与尾部半截串做「首（声母）/尾（韵母）替换」，
        // 每处替换独立成一路查询，结果按文本去重合并。fuzzy_map 为空时整块跳过。
        if !self.fuzzy_map.is_empty() {
            let mut seen: std::collections::HashSet<String> =
                cands.iter().map(|c| c.text.clone()).collect();
            let push_variant = |reading: &[String],
                                tail: &str,
                                seen: &mut std::collections::HashSet<String>,
                                cands: &mut Vec<Candidate>| {
                if let Ok(mut vc) =
                    self.dict
                        .lookup_prefix(reading, tail, self.config.candidate_limit)
                {
                    vc.retain(|c| seen.insert(c.text.clone()));
                    cands.extend(vc);
                }
            };
            // 完整音节变体
            for (i, syl) in reading.iter().enumerate() {
                for v in fuzzy_expand(&self.fuzzy_map, syl) {
                    let mut variant = reading.clone();
                    variant[i] = v;
                    push_variant(&variant, &tail, &mut seen, &mut cands);
                    if cands.len() >= self.config.candidate_limit {
                        break;
                    }
                }
            }
            // 尾部（半截或完整）变体
            for v in fuzzy_expand(&self.fuzzy_map, &tail) {
                push_variant(&reading, &v, &mut seen, &mut cands);
                if cands.len() >= self.config.candidate_limit {
                    break;
                }
            }
            cands.truncate(self.config.candidate_limit);
        }

        // 多切分查询（蛋糕 bug）：只查首切分会漏掉整个正确切分——"dangao" 贪心切成
        // ["dang","ao"]，真词「蛋糕」只在 ["dan","gao"] 里。segment() 的切分数有界
        // （≤12 字母实测 2–4 条，Fibonacci 级），每条同样带 candidate_limit 查询；
        // 结果按文本去重后**续在首切分之后**——首切分总序零变化（preedit/层一/英文
        // 层一的落位契约都不受影响），重复文本保留频率更高的出现。
        let mut seen_at: std::collections::HashMap<String, usize> = cands
            .iter()
            .enumerate()
            .map(|(i, c)| (c.text.clone(), i))
            .collect();
        for extra in segs.iter().skip(1) {
            let mut r = extra.clone();
            let t = r.pop().unwrap_or_default();
            if let Ok(vc) = self.dict.lookup_prefix(&r, &t, self.config.candidate_limit) {
                for c in vc {
                    match seen_at.get(&c.text).copied() {
                        Some(i) => cands[i].freq = cands[i].freq.max(c.freq),
                        None => {
                            seen_at.insert(c.text.clone(), cands.len());
                            cands.push(c);
                        }
                    }
                }
            }
        }
        // 邻键纠错：直查（主路径/模糊/多切分）候选不足时，对按键串生成编辑距离 1
        // 变体（邻键替换 + 相邻转位）重查。变体复用整条现有管线（segment →
        // lookup_prefix）；纠错候选续在精确结果之后（seen_at 去重，不抢精确的位）。
        // correction=false 或候选充足时零开销。放在 abbrev 兜底之前：先纠错、
        // 纠不中再落缩写垃圾。
        // 长度门控：纠错的职责是「词级输入打错键」（≤12 字母 = 6 音节全拼）。
        // 长句直查候选天然少，纠错枚举只会在 Viterbi 马上要接管的场景白烧 ——
        // 实测 27 字母长句逐键 4.4ms → 门控后 0.2ms（correction=false 同级）。
        if self.config.correction
            && self.letters.len() <= crate::correction::MAX_CORRECTION_INPUT
            && cands.len() < crate::correction::CORRECTION_TRIGGER_MIN
        {
            let mut corrected_seen: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            let baseline = segs.first().cloned().unwrap_or_default();
            for fixed in crate::correction::corrected_keys(&self.letters) {
                // 廉价预筛（qingjian 同款）：变体绝大多数仍是非法串，先用零分配
                // 可达性 DP 挡掉，幸存的极少数才进完整 segment + lookup。
                if !kime_pinyin::is_fully_segmentable(&fixed) {
                    continue;
                }
                let Some(full) = segment(&fixed).into_iter().next() else {
                    continue;
                };
                // 与原切分完全相同的变体是白查（替换后同音节），跳过
                if full == baseline {
                    continue;
                }
                let mut r = full;
                let t = r.pop().unwrap_or_default();
                if let Ok(vc) = self.dict.lookup_prefix(&r, &t, self.config.candidate_limit) {
                    for c in vc {
                        if corrected_seen.insert(c.text.clone()) && !seen_at.contains_key(&c.text) {
                            seen_at.insert(c.text.clone(), cands.len());
                            cands.push(c);
                        }
                    }
                }
                if cands.len() >= self.config.candidate_limit {
                    break;
                }
            }
        }
        // 若全部切分的主路径与模糊路径都无候选，回退到缩写查询
        if cands.is_empty() {
            if let Ok(ab) = self
                .dict
                .lookup_abbrev(&self.letters, self.config.candidate_limit)
            {
                cands = ab;
            }
            self.last_reading.clear();
        }
        // 句级联想（M8/Task 5）：如果有完整音节切分且长度 >= 2，尝试通过 Viterbi 构词成句
        if let Some(full_reading) = segs.first() {
            if full_reading.len() >= 2 {
                let seed = self.context_seed();
                let sentences = crate::lattice::viterbi_sentences_seeded(
                    &self.dict,
                    full_reading,
                    seed,
                    &mut self.span_cache,
                );
                // 全拼路径：full_reading == reading + [tail]，句子读音恒等于 joined。
                Self::place_sentences(&mut cands, sentences, &self.last_joined);
            }
        }
        // 缺陷 A 回退：主路径/模糊/多切分/纠错/缩写/整句全空时砍末音节逐档重查。
        // 用**整串的首切分**音节序列（= 用户敲出的完整读音），无完整切分则无回退空间。
        if cands.is_empty() {
            if let Some(fb) = segs.first().and_then(|f| self.longest_prefix_candidates(f)) {
                cands = fb;
            }
        }
        // 长串降级：候选被整句/长组合词占满但不足一页时，把首音节的单字候选**追加到
        // 末尾**——整句在前、单字在后，用户翻页翻得到「我」这类首音节的字，选它继续
        // 组词。只追加、按文本去重、总量截到 candidate_limit：层一精确命中块与
        // place_sentences 落好的整句名次零影响，空格首选永远是层一最优候选。
        if !cands.is_empty() {
            if let Some(full) = segs.first() {
                Self::append_first_syllable_candidates(
                    &mut cands,
                    &self.dict,
                    full,
                    self.config.candidate_limit,
                    false,
                );
            }
        }
        self.candidates = cands;
    }

    /// 缺陷 A：整串无词条、补全/缩写/Viterbi 全都拼不出候选时的最后一道回退——
    /// 砍掉末音节逐档重查（`[..n-1]` → `[..1]`），取第一个非空结果返回。
    /// 用户至少能看到首音节的字/词可选；`preedit` / `last_reading` / `last_joined`
    /// 全部保持整串原样（回退只补候选列表，不改显示与读音语义）。
    /// 不足一个音节或逐档全空 → `None`。纯查询，不改引擎状态。
    /// k 从完整长度起：双拼半截键场景下 syllables 是**已凑齐的音节**，末键在
    /// pending 里——第一档就等于「丢掉 pending 重查」，单音节也能回退（`ni`+pending
    /// `q` → 回退查 `ni` 本身）。全拼无 pending 时第一档与主查询重复，必空，直接进下一档。
    fn longest_prefix_candidates(&self, syllables: &[String]) -> Option<Vec<Candidate>> {
        if syllables.is_empty() {
            return None;
        }
        for k in (1..=syllables.len()).rev() {
            let cands = self
                .dict
                .lookup_prefix(&syllables[..k], "", self.config.candidate_limit)
                .unwrap_or_default();
            if !cands.is_empty() {
                return Some(cands);
            }
        }
        None
    }

    /// 长串降级（用户诉求）：**长串**（≥3 音节）拼音的候选被整句/长组合词占满但
    /// 不足一页时，把首音节的单字候选追加到列表末尾（整句在前、单字在后，与
    /// 「中文在前英文在后」的落位约定一致——英文块由 `merge_english` 之后统一
    /// 追加，仍垫底）。用户翻页翻得到「我」这类首音节的字，选它继续组词。
    ///
    /// 长度门控是硬契约：1–2 音节输入（`wo` / `zaishuo` 这类「一个词」）的候选
    /// 列表逐字不变——精确命中 + 补全本就够用，单字塞进来只会污染短串行为
    /// （`tests/context_seed_test.rs` 钉死了两音节输入的完整候选列表）。
    ///
    /// 只做追加：按文本去重、总量截到 limit，层一精确命中块与 `place_sentences`
    /// 落好的整句名次零变化，空格首选永远是层一最优候选。
    /// `syllables` = 整串首切分读音（与 `longest_prefix_candidates` 同源）；
    /// 音节不足 3 个、候选已满一页（>= page_size）或无完整切分 → 不动作。
    fn append_first_syllable_candidates(
        cands: &mut Vec<Candidate>,
        dict: &Dict,
        syllables: &[String],
        limit: usize,
        force: bool,
    ) {
        if !force && syllables.len() < 3 || limit == 0 || syllables.is_empty() {
            return;
        }
        let Ok(mut extra) = dict.lookup_prefix(&syllables[..1], "", limit) else {
            return;
        };
        extra.retain(|c| !cands.iter().any(|o| o.text == c.text));
        // 预算独立于主查询的 candidate_limit：之前 truncate 到「剩余空间」，
        // 主查询占满 50 时剩余 0 → max(1) 只追加 1 个单字（zhongguoren 实测
        // 只剩 1 个「中」）。参照 qingjian/fcitx5：候选总量可远超一页，首音节
        // 单字全量进同一列表，翻页在平台层做。extra 本身已被 lookup 的 limit
        // 约束为 freq 降序的前 limit 条，追加在末尾不污染层一/整句名次。
        cands.extend(extra);
    }
    /// 由上下文尾巴反查「上文末词」的读音，作为整句联想的种子（上下文感知）。
    ///
    /// 窗口 = 光标前末 1..=4 字：先试 4 字词、再 3、2、1，取频率最高的命中作种子
    /// （`readings_of_text` 每个长度内已按 freq DESC，这里跨长度取最大）。全部 miss
    /// → `None`（无种子，`viterbi_sentences_seeded` 退化为旧行为）。每键最多 4 次
    /// `idx_phrase_text` 点查（O(log n)）——lattice 自身已是 O(n²) 次查词，这里不构成
    /// 新瓶颈；没加 (tail → seed) 缓存：单次索引点查在微秒级，缓存省不回它的复杂度。
    ///
    /// ASCII 上下文（英文/数字）直接 `None`：latin 无 lattice，且 merge_english 的
    /// 层一排序必须与无上下文完全一致（回归测试钉住）。纯中文标点尾巴查无命中，
    /// 同样落 `None`。
    fn context_seed(&self) -> Option<Seed> {
        let tail = self.context_tail()?;
        // 末 4 字窗口（按字符，非字节）：上文末词至多按 4 字词反查。
        let chars: Vec<char> = tail.chars().collect();
        let window: String = chars[chars.len().saturating_sub(4)..].iter().collect();
        if window.bytes().any(|b| b.is_ascii()) {
            return None;
        }
        let mut best: Option<(String, u64)> = None;
        let mut text: &str = &window;
        while !text.is_empty() {
            if let Ok(hits) = self.dict.readings_of_text(text, 4) {
                if let Some((reading, freq)) = hits.into_iter().next() {
                    match &best {
                        Some((_, bf)) if *bf >= freq => {}
                        _ => best = Some((reading, freq)),
                    }
                }
            }
            // 砍掉窗口首字符：4 → 3 → 2 → 1，窗口始终贴着光标。
            let next = text
                .char_indices()
                .nth(1)
                .map(|(i, _)| i)
                .unwrap_or(text.len());
            text = &text[next..];
        }
        best.map(|(reading, freq)| Seed { reading, freq })
    }

    /// 整句候选名次（工单第 4 条）：覆盖全部输入音节的句子属层一——
    /// **永不压过层一的精确命中词**（ln(freq) 可加的代价模型里「两个超高频单字」
    /// 永远比一个真词便宜，`ufme`→神么 压过 什么 是实测过的回归），
    /// 但在补全区里按自身频率（`lattice::sentence_score` 的联合概率折算值）落位：
    /// 精确块为空时，覆盖句因此排进 `我们确信`/`最近好吗` 这类
    /// 「还要继续敲」的补全之前，而不是无条件钉死在列表末尾。
    /// 没覆盖全输入的句子（双拼半截键 pending）不参与落位，挂末尾。
    fn place_sentences(cands: &mut Vec<Candidate>, sentences: Vec<Candidate>, joined: &str) {
        let l1_end = cands.iter().take_while(|c| c.pinyin == joined).count();
        // k-best 列表本身按路径代价升序；后续句子只许插在前一条之后，
        // 否则频率折算会把更优的切分反-sort 到后面。
        let mut cursor = l1_end;
        for sentence in sentences {
            if cands.iter().any(|c| c.text == sentence.text) {
                continue;
            }
            let pos = if sentence.pinyin == joined {
                // 补全区是 freq 降序的：落在第一个比它弱的候选之前；层一永不被越过。
                cursor
                    + cands[cursor..]
                        .iter()
                        .position(|c| c.freq < sentence.freq)
                        .unwrap_or(cands.len() - cursor)
            } else {
                cands.len()
            };
            cands.insert(pos, sentence);
            cursor = pos + 1;
        }
    }

    /// M5: AI 候选合入当前列表。后台线程完成后由壳回调（仍在主线程执行）
    pub fn merge_ai(&mut self, ai: Vec<Candidate>) {
        let mut seen = std::collections::HashSet::new();
        // 先加入现有候选文本集合
        for c in &self.candidates {
            seen.insert(c.text.clone());
        }
        // 去重合并 AI 候选
        for c in ai {
            if seen.insert(c.text.clone()) {
                self.candidates.push(c);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kime_shuangpin::Scheme;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};
    fn tmp_db(suffix: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "kime_engine_{}_{}_{}.sqlite",
            suffix,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_file(&path);
        path
    }

    fn fixture_yaml() -> &'static str {
        // 简洁但覆盖多音节 / 单字 / 半截尾命中路径：
        // - "你好" / "泥猴" 都以 "ni" 开头，便于测试半截尾 "h" → "ha"/"hou"。
        // - "世界" 测全拼完整匹配。
        // - "安" 单字 pinyin 测空 syllables + tail。
        "\
...\n你好\tni hao\t5000\n\
泥猴\tni hou\t100\n\
世界\tshi jie\t9999\n\
安\ta\t8000\n\
啊啊\ta a\t1\n\
"
    }

    fn fixture_yaml_path() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "kime_engine_yaml_{}_{}.yaml",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&p, fixture_yaml()).unwrap();
        p
    }

    fn engine_with_fixture() -> (Engine, std::path::PathBuf, std::path::PathBuf) {
        let db = tmp_db("main");
        let yaml = fixture_yaml_path();
        let _dict = Dict::open(&db).expect("open dict");
        // 直接 INSERT 行：fixture_yaml 可能因格式敏感失败，回退到 raw SQL。
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "INSERT OR REPLACE INTO phrase(pinyin, text, freq, abbrev, user) VALUES
               ('ni''hao','你好',5000,'nh',0),
               ('ni''hou','泥猴', 100,'nh',0),
               ('shi''jie','世界',9999,'sj',0),
               ('a',     '安',  8000,'a', 0),
               ('a''a',  '啊啊',   1,'a', 0);
            ",
        )
        .unwrap();
        drop(conn);
        // dict connection is stale now; reopen for lookups.
        let dict = Dict::open(&db).expect("reopen dict for lookups");
        let config = Config {
            shuangpin: None,
            ..Config::default()
        };
        let engine = Engine::new(dict, config);
        (engine, db, yaml)
    }

    fn engine_with_shuangpin_fixture() -> (Engine, std::path::PathBuf, std::path::PathBuf) {
        let db = tmp_db("shuangpin");
        let yaml = fixture_yaml_path();
        let _dict = Dict::open(&db).expect("open dict");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "INSERT OR REPLACE INTO phrase(pinyin, text, freq, abbrev, user) VALUES
               ('ni''hao','你好',5000,'nh',0),
               ('ni''hou','泥猴', 100,'nh',0),
               ('shi''jie','世界',9999,'sj',0),
               ('a',     '安',  8000,'a', 0),
               ('a''a',  '啊啊',   1,'a', 0),
               ('ni''hao','你好',5000,'h%',0),
               ('ni''hao','你好',5000,'x',0),
               ('fan''gan','反感',600,'fg',0),
               ('fang''an','方案',900,'fa',0);
            ",
        )
        .unwrap();
        drop(conn);
        let dict = Dict::open(&db).expect("reopen dict for lookups");
        let config = Config {
            dict_path: db.to_string_lossy().to_string(),
            shuangpin: Some(Scheme::Xiaohe),
            ..Config::default()
        };
        let engine = Engine::new(dict, config);
        (engine, db, yaml)
    }
    fn k(ch: char) -> Key {
        Key {
            ch: Some(ch),
            code: 0,
            shift: false,
            ctrl: false,
            alt: false,
        }
    }

    fn code_k(code: u32) -> Key {
        Key {
            ch: None,
            code,
            shift: false,
            ctrl: false,
            alt: false,
        }
    }

    fn shift_k(code: u32) -> Key {
        Key {
            ch: None,
            code,
            shift: true,
            ctrl: false,
            alt: false,
        }
    }

    /// C-h 光标编辑键（Ctrl+H，evdev 码 35）。
    fn ctrl_h_k() -> Key {
        Key {
            ch: Some('h'),
            code: 35,
            shift: false,
            ctrl: true,
            alt: false,
        }
    }

    #[test]
    fn shuangpin_even_length_input() {
        let (mut e, db, yaml) = engine_with_shuangpin_fixture();
        // 小鹤码 nihc -> ni + hao
        for c in "nihc".chars() {
            e.key(k(c));
        }
        // preedit 应为拼音 "nihao"
        assert_eq!(e.preedit(), "nihao");
        // last_reading 应为 ["ni", "hao"]
        assert_eq!(e.last_reading, vec!["ni".to_string(), "hao".to_string()]);
        // 候选应含 "你好"
        let cs: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
        assert!(cs.contains(&"你好".to_string()));
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn shuangpin_odd_length_input() {
        let (mut e, db, yaml) = engine_with_shuangpin_fixture();
        // nih -> ni + "h" 尾部前缀
        for c in "nih".chars() {
            e.key(k(c));
        }
        // preedit 应为拼音 "ni" + tail "h"
        assert_eq!(e.preedit(), "nih");
        // last_reading 应为 ["ni"]
        assert_eq!(e.last_reading, vec!["ni".to_string()]);
        // tail "h" 应触发 "ha" 前缀匹配，候选应含 "你好"
        let cs: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
        assert!(cs.contains(&"你好".to_string()));
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn digit_selection_triggers_learn_and_reorders_candidates() {
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nih".chars() {
            e.key(k(c));
        }
        // 1 选词 "你好"（#81 起入 pending，不上屏），应触发 learn -> 候选顺序变化
        let outcome = e.key(k('1'));
        match outcome {
            Outcome::Consumed => {}
            other => panic!("expected Consumed(pending), got {:?}", other),
        }
        // 再查询，确保排序已更新（用户词频率更高）
        let mut e2 = Engine::new(Dict::open(&db).unwrap(), Config::default());
        for c in "nih".chars() {
            e2.key(k(c));
        }
        // 现在 "你好" 频率高，排在第一
        assert_eq!(e2.candidates().first().unwrap().text, "你好");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn space_selection_triggers_learn_and_clears() {
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nih".chars() {
            e.key(k(c));
        }
        // #81：空格选词 = 入 pending（不上屏），再按空格整段释放
        assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
        assert_eq!(e.preedit(), "你好", "pending 进显示串");
        assert!(e.candidates().is_empty());
        match e.key(code_k(KEY_SPACE)) {
            Outcome::Commit(t) => assert_eq!(t, "你好"),
            other => panic!("expected 释放 Commit, got {:?}", other),
        }
        assert!(e.preedit().is_empty(), "释放后显示串清空");
        assert!(e.candidates().is_empty());
        // 学习后再次查询，用户词频率更高
        let mut e2 = Engine::new(Dict::open(&db).unwrap(), Config::default());
        for c in "nih".chars() {
            e2.key(k(c));
        }
        assert_eq!(e2.candidates().first().unwrap().text, "你好");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn full_pinyin_abbrev_fallback() {
        let (mut e, db, yaml) = engine_with_fixture();
        // 打 "nh" -> abbrev 匹配 "nh" 对应 "你好"/"泥猴"
        for c in "nh".chars() {
            e.key(k(c));
        }
        // abbrev 兜底时 last_reading 应清空
        assert!(e.last_reading.is_empty());
        // 候选应含 "你好"
        let cs: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
        assert!(cs.contains(&"你好".to_string()));
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    /// #83：双拼模式下空组合 Shift = 空操作（无持久模式），拼音照常可打
    /// （旧用例断言「Shift 切英文后双拼停用」——模式已删）。
    #[test]
    fn shift_empty_is_noop_shuangpin() {
        let (mut e, db, yaml) = engine_with_shuangpin_fixture();
        assert_eq!(e.key(shift_k(KEY_LEFTSHIFT)), Outcome::Consumed);
        assert!(e.chinese());
        assert_eq!(e.key(k('n')), Outcome::Consumed, "双拼音照常累积");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn shuangpin_preedit_shows_decoded_pinyin() {
        let (mut e, db, yaml) = engine_with_shuangpin_fixture();
        // 打 "nihc" -> 小鹤解码 nihao
        for c in "nihc".chars() {
            e.key(k(c));
        }
        // preedit 应为解码出的拼音 "nihao"，而不是原字母 "nihc"
        assert_eq!(e.preedit(), "nihao");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn accumulates_letters_into_preedit_and_candidates() {
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nih".chars() {
            assert_eq!(e.key(k(c)), Outcome::Consumed);
        }
        assert_eq!(e.preedit(), "nih");
        let cs: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
        assert!(
            cs.contains(&"你好".to_string()),
            "half-tail 'h' should match ni'hao, got {:?}",
            cs
        );
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn half_syllable_shows_prefix_candidates() {
        // "niha" → 切 ["ni","ha"]；按规则 tail = "ha"、reading = ["ni"]，
        // 所以 LIKE 'ni''ha%' 应命中 "ni'hao" → "你好"。
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "niha".chars() {
            e.key(k(c));
        }
        assert_eq!(e.preedit(), "niha");
        let cs: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
        assert!(
            cs.contains(&"你好".to_string()),
            "niha should still surface 你好 via ni'ha% prefix, got {:?}",
            cs
        );
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn digit_selects_candidate_and_clears() {
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nih".chars() {
            e.key(k(c));
        }
        let outcome = e.key(k('1'));
        match outcome {
            Outcome::Consumed => {}
            other => panic!("expected Consumed(pending), got {:?}", other),
        }
        assert_eq!(e.preedit(), "你好", "数字选词入 pending（#81）");
        assert!(e.candidates().is_empty());
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn space_commits_top_candidate() {
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nih".chars() {
            e.key(k(c));
        }
        // #81：空格选词入 pending，再按空格整段释放
        assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
        assert_eq!(e.preedit(), "你好");
        match e.key(code_k(KEY_SPACE)) {
            Outcome::Commit(t) => assert_eq!(t, "你好"),
            other => panic!("expected 释放 Commit, got {:?}", other),
        }
        assert!(e.preedit().is_empty());
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn digit_without_candidates_is_ignored() {
        let (mut e, db, yaml) = engine_with_fixture();
        assert_eq!(e.key(k('1')), Outcome::Ignored);
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn space_without_candidates_is_ignored() {
        let (mut e, db, yaml) = engine_with_fixture();
        assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Ignored);
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn backspace_removes_last_letter() {
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nih".chars() {
            e.key(k(c));
        }
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Consumed);
        assert_eq!(e.preedit(), "ni");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn backspace_on_empty_is_ignored() {
        let (mut e, db, yaml) = engine_with_fixture();
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Ignored);
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    /// 部分选词（选词只消耗已选音节）后光标必须落在剩余拼音末尾：
    /// 后续打字续在尾部而不是插进剩余拼音头部（2.1「光标不是最后」回归）。
    #[test]
    fn partial_selection_keeps_caret_at_tail() {
        let (mut e, db, yaml) = engine_with_fixture();
        // nihaoni：[ni,hao,ni] 分段，唯一候选 你好（fixture 无单字「你」），
        // 选词消耗 ni+hao，剩 "ni"
        for c in "nihaoni".chars() {
            e.key(k(c));
        }
        // #81：空格选词入 pending（不上屏），显示串 = pending + 剩余拼音
        assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
        assert_eq!(e.letters, "ni", "剩余拼音必须保留");
        assert_eq!(e.preedit(), "你好ni", "显示串含预选词");
        assert_eq!(e.cursor(), e.letters.len(), "光标在剩余拼音末尾");
        assert_eq!(e.preedit_cursor(), 4, "caret = 你好(2) + ni(2)");
        e.key(k('h'));
        assert_eq!(e.letters, "nih", "新字符续在剩余拼音尾部");
        assert_eq!(e.preedit(), "你好nih");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    /// #79：全量选词上屏后撤销链**保留**（#76 规则作废）——退格弹整词：
    /// 拼音还原进组合 + UndoApp(n-1) 让壳删掉上屏的 n 个字，回退统一。
    #[test]
    fn full_selection_keeps_undo_chain() {
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nihao".chars() {
            e.key(k(c));
        }
        // #81：第一次空格 = 选词入 pending（不上屏）；第二次 = 整段释放
        assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
        assert_eq!(e.preedit(), "你好", "pending 显示");
        match e.key(code_k(KEY_SPACE)) {
            Outcome::Commit(t) => assert_eq!(t, "你好"),
            other => panic!("expected 释放 Commit, got {:?}", other),
        }
        assert!(e.letters.is_empty());
        assert_eq!(e.undo_depth(), 1, "全量选词上屏，撤销链跨上屏存活");
        // 退格 = 弹整词：拼音还原 + 壳删 2 个上屏字（物理 1 + 合成 1）
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::UndoApp(1));
        assert_eq!(e.letters, "nihao", "整词弹回组合，可重新选词/组词");
        assert_eq!(e.undo_depth(), 0);
        // 栈净后逐字删拼音
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Consumed);
        assert_eq!(e.letters, "niha");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    /// #81 预选 + #80 LIFO：连选两词（你好 + 泥猴）入 pending；释放前
    /// 退格 = 逐词弹出（拼音弹回可重组，应用零删除）；释放后 = LIFO
    /// 弹回 + 应用级删除。
    #[test]
    fn two_word_chain_pops_in_lifo_order() {
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nihao".chars() {
            e.key(k(c));
        }
        // 选词入 pending（#81：不上屏）
        assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
        assert_eq!(e.preedit(), "你好");
        for c in "nihou".chars() {
            e.key(k(c));
        }
        assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
        assert_eq!(e.preedit(), "你好泥猴", "两词都在 pending");
        assert!(e.letters.is_empty());
        assert_eq!(
            e.undo_depth(),
            2,
            "两次选词 = 两条栈条目（与 pending 配对）"
        );
        // 释放前退格 = 弹最近词（泥猴）：拼音弹回、应用零删除
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Consumed);
        assert_eq!(e.letters, "nihou", "弹词拼音可重组");
        assert_eq!(e.undo_depth(), 1);
        // 再弹 = 你好：整串还原
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Consumed);
        assert_eq!(e.letters, "nihaonihou", "整串可重新组词");
        assert_eq!(e.undo_depth(), 0);
        // 再选（fixture 整句候选 你好泥猴 为首，#80 钉死）+ 整段释放
        assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
        match e.key(code_k(KEY_SPACE)) {
            Outcome::Commit(t) => assert_eq!(t, "你好泥猴"),
            other => panic!("expected 释放 Commit, got {:?}", other),
        }
        // 释放后 LIFO：弹整句 + 壳删 4 个上屏字（物理 1 + 合成 3）
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::UndoApp(3));
        assert_eq!(e.letters, "nihaonihou", "拼音弹回组合");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    /// #77 撤销优先 + #79 统一 undo：部分选词后第一下退格 = 撤销最近
    /// 选词（消耗拼音 + 剩余整串还原，UndoApp 删上屏字），不删拼音；
    /// 栈净后逐字删拼音，双空放行应用。
    #[test]
    fn backspace_partial_selection_undoes_in_order() {
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nihaoni".chars() {
            e.key(k(c));
        }
        // #81：选词入 pending（不上屏）
        assert!(matches!(e.key(code_k(KEY_SPACE)), Outcome::Consumed));
        assert_eq!(e.letters, "ni");
        assert_eq!(e.preedit(), "你好ni");
        // 释放前退格 = 弹选词（纯内部，应用零删除；拼音弹回）
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Consumed);
        assert_eq!(e.letters, "nihaoni", "弹词：消耗段 + 剩余 = 原串");
        assert_eq!(e.undo_depth(), 0, "弹词同序弹掉无效栈条目");
        // 栈净 → 删拼音逐字
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Consumed);
        assert_eq!(e.letters, "nihaon");
        // 删净剩余（6 字）
        for _ in 0..6 {
            assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Consumed);
        }
        assert!(e.letters.is_empty());
        // 双空 = 空闲态，退格放行删应用字符
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Ignored);
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    /// #77 撤销优先 + #79：双拼部分选词，撤销弹回的是**键位串**（非拼音），
    /// 壳按上屏字数（你好 = 2）补发合成退格；栈净后逐键删除，删到键位串
    /// （nihc）后双拼解码重建 preedit。尾段 zz 无 fixture 词 → 候选只有你好。
    #[test]
    fn shuangpin_backspace_undo_restores_keys() {
        let (mut e, db, yaml) = engine_with_shuangpin_fixture();
        for c in "nihczz".chars() {
            e.key(k(c));
        }
        // #81：选词入 pending（不上屏）
        assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
        assert_eq!(e.letters, "zz", "部分选词剩键位串");
        assert_eq!(e.preedit(), "你好zz");
        // 释放前退格 = 弹词（键位串弹回，应用零删除）
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Consumed);
        assert_eq!(e.letters, "nihczz", "弹词还原键位串");
        assert_eq!(e.undo_depth(), 0, "弹词同序清掉无效条目");
        // 再选 + 整段释放
        assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
        match e.key(code_k(KEY_SPACE)) {
            Outcome::Commit(t) => assert_eq!(t, "你好"),
            other => panic!("expected 释放 Commit, got {:?}", other),
        }
        // 释放后 LIFO：弹整词 + 删 2 上屏字
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::UndoApp(1));
        assert_eq!(e.letters, "nihczz", "双拼撤销弹回键位串");
        assert_eq!(e.undo_depth(), 0);
        // 栈净 → 逐键删到 nihc，双拼解码重建 preedit
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Consumed);
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Consumed);
        assert_eq!(e.letters, "nihc");
        assert_eq!(e.preedit(), "nihao", "弹回后按双拼解码重建 preedit");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    /// #77 推翻 #70：Esc 清组合（壳层 DEACTIVATE 合成 Esc 同此路径）=
    /// 撤销链终结——面板关闭后退格放行应用，不弹旧拼音。
    /// 前置必须是部分选词（全量选词已在 select_candidate 终止链路）。
    #[test]
    fn esc_clears_undo_stack() {
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nihaoni".chars() {
            e.key(k(c));
        }
        // #81：选词入 pending；Esc 取消整句（含 pending），栈随组合清
        assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
        assert_eq!(e.letters, "ni", "部分选词剩拼音，撤销链在栈上");
        assert_eq!(e.undo_depth(), 1);
        // Esc 清掉剩余组合——#77 起同步清栈（#70 旧契约推翻）
        assert_eq!(e.key(code_k(KEY_ESC)), Outcome::Consumed);
        assert!(e.preedit().is_empty());
        assert_eq!(e.undo_depth(), 0, "组合清 = 撤销链终结");
        // 空闲态：退格放行应用，不弹旧拼音
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Ignored);
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    /// #79：C-h 把剩余拼音删空**不清**撤销链（#77 不变式作废）——
    /// 删空后链仍在，退格照样弹上屏字，回退统一。
    #[test]
    fn ctrl_h_delete_to_empty_keeps_undo_chain() {
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nihaoni".chars() {
            e.key(k(c));
        }
        // #81：选词入 pending（不上屏）
        assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
        assert_eq!(e.letters, "ni");
        assert_eq!(e.undo_depth(), 1);
        // C-h 逐字删（光标在尾）：删空也不清链
        assert_eq!(e.key(ctrl_h_k()), Outcome::Consumed);
        assert_eq!(e.letters, "n");
        assert_eq!(e.key(ctrl_h_k()), Outcome::Consumed);
        assert!(e.letters.is_empty());
        assert_eq!(e.undo_depth(), 1, "删空不清链（#79）");
        // 空格释放整段
        match e.key(code_k(KEY_SPACE)) {
            Outcome::Commit(t) => assert_eq!(t, "你好"),
            other => panic!("expected 释放 Commit, got {:?}", other),
        }
        // 释放后退格 = 弹词：拼音还原 + 壳删 2 字
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::UndoApp(1));
        assert_eq!(e.letters, "nihao");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    /// Shift-flush 路径（#83 起不切模式）：letters 非空时 Shift 走
    /// Commit(**原字母**) + 清组合——输入串随原样上屏结束，词级撤销链
    /// 随 burst 终止；模式保持中文，退格放行给应用，不再弹回旧选词拼音
    /// （v0.19.68 前语义是保留，3eb8279 的 `shift_flush_keeps_undo_stack`
    /// 语义反转；#83 再删掉切英文半边）。
    #[test]
    fn shift_flush_ends_undo_chain() {
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nihaoni".chars() {
            e.key(k(c));
        }
        // #81：选词入 pending
        assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
        assert_eq!(e.letters, "ni", "部分选词剩拼音");
        assert_eq!(e.undo_depth(), 1);
        // 剩余组合在、直接 Shift：pending + 原字母整段上屏 + 清组合，burst 结束
        match e.key(shift_k(KEY_LEFTSHIFT)) {
            Outcome::Commit(t) => assert_eq!(t, "你好ni", "shift-flush 连 pending 冲刷"),
            other => panic!("expected Commit(你好ni), got {:?}", other),
        }
        assert!(e.chinese(), "#83 起 Shift 不切模式");
        assert!(e.letters.is_empty());
        assert_eq!(
            e.undo_depth(),
            0,
            "shift-flush 上屏即输入串结束，撤销链终止"
        );
        // 退格放行，不再弹旧词
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Ignored);
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    /// 标点上屏 = 输入串结束：部分选词剩组合时接标点（情况 B 顶字+标点），
    /// 撤销链随 burst 终止；#85：智能标点转换后退格 = PuncCancel 还原原键。
    #[test]
    fn punct_commit_ends_undo_chain() {
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nihao".chars() {
            e.key(k(c));
        }
        // #81：选词入 pending（nihao 完整消耗，letters 空）；标点 = 情况 A
        // （letters 空 + pending 非空）：释放 pending + 标点，硬终结点
        assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
        assert!(e.letters.is_empty());
        assert_eq!(e.preedit(), "你好", "pending 显示");
        assert_eq!(e.undo_depth(), 1);
        // 标点上屏终结 burst
        match e.key(k('.')) {
            Outcome::Commit(t) => assert_eq!(t, "你好。", "pending 释放 + 标点"),
            other => panic!("expected Commit, got {:?}", other),
        }
        assert_eq!(e.undo_depth(), 0, "标点 Commit 即输入串结束，撤销链终止");
        // #85：空闲退格先消费「末次标点撤销」，不直接放行应用
        assert_eq!(
            e.key(code_k(KEY_BACKSPACE)),
            Outcome::PuncCancel {
                original: ".".into(),
                fullwidth: "。".into()
            },
            "栈已清，退格先撤销智能标点转换"
        );
        assert_eq!(
            e.key(code_k(KEY_BACKSPACE)),
            Outcome::Ignored,
            "撤销已消费，再退格放行"
        );
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }
    /// 弹选词删净剩余拼音后撤销链自净（#80 LIFO）；#83 起空组合
    /// Shift = 无动作（无翻转、不清栈、不切模式），拼音照常可打。
    #[test]
    fn backspace_to_empty_no_mode_path() {
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nihao".chars() {
            e.key(k(c));
        }
        // #81：选词入 pending → 再空格释放
        assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
        match e.key(code_k(KEY_SPACE)) {
            Outcome::Commit(t) => assert_eq!(t, "你好"),
            other => panic!("expected 释放 Commit, got {:?}", other),
        }
        // #77 撤销优先 + #79：第一下退格弹选词（还原原串、栈净、删上屏字）
        assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::UndoApp(1));
        assert_eq!(e.letters, "nihao", "弹选词还原原串");
        for _ in 0..5 {
            assert_eq!(e.key(code_k(KEY_BACKSPACE)), Outcome::Consumed);
        }
        assert!(e.letters.is_empty());
        assert_eq!(e.undo_depth(), 0, "链已随弹栈净");
        // 空 letters：Shift 无动作（#83：无持久模式），拼音照常可打
        assert_eq!(e.key(shift_k(KEY_LEFTSHIFT)), Outcome::Consumed);
        assert!(e.chinese(), "无路径能切英文模式");
        assert_eq!(e.key(k('n')), Outcome::Consumed, "中文模式照常吃拼音");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    /// #83：空组合 Shift = 空操作（轻点锁死根子删除）——模式恒中文。
    #[test]
    fn shift_tap_is_noop() {
        let (mut e, db, yaml) = engine_with_fixture();
        assert!(e.chinese());
        for _ in 0..3 {
            assert_eq!(e.key(shift_k(KEY_LEFTSHIFT)), Outcome::Consumed);
        }
        assert!(e.chinese(), "轻点 Shift 不切模式");
        assert_eq!(e.key(k('n')), Outcome::Consumed, "拼音照常");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    /// Shift 冲刷（原串上屏）后模式保持中文，后续按键照常进拼音
    /// （旧断言「切英文后字母 Ignored」已随 #83 删除）。
    #[test]
    fn shift_with_active_composition_commits_raw() {
        let (mut e, db, yaml) = engine_with_fixture();
        e.key(k('n'));
        let mut alt_n = k('n');
        alt_n.alt = true;
        assert_eq!(e.key(alt_n), Outcome::Ignored);
        assert_eq!(e.preedit(), "n");
        e.key(k('h'));
        assert!(e.chinese());
        assert_eq!(e.key(shift_k(KEY_LEFTSHIFT)), Outcome::Commit("nh".into()));
        assert!(e.chinese(), "#83 起冲刷不切模式");
        assert!(e.preedit().is_empty());
        assert_eq!(e.key(k('a')), Outcome::Consumed, "仍是中文拼音");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn complete_pinyin_finds_exact_candidate() {
        // "nihao" → 完整双字拼音，应命中 "你好"。
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nihao".chars() {
            e.key(k(c));
        }
        let cs: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
        assert!(
            cs.contains(&"你好".to_string()),
            "completed pinyin 'nihao' should surface 你好, got {:?}",
            cs
        );
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn single_letter_pinyin_returns_candidates_starting_with_it() {
        // "a" 单独成音节 → tail="a", reading=[] → LIKE 'a%' 命中 "安" / "啊啊"。
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "a".chars() {
            e.key(k(c));
        }
        let cs: Vec<String> = e.candidates().iter().map(|c| c.text.clone()).collect();
        assert!(
            cs.contains(&"安".to_string()),
            "a should match 安, got {:?}",
            cs
        );
        assert!(
            cs.contains(&"啊啊".to_string()),
            "a should match 啊啊, got {:?}",
            cs
        );
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn invalid_pinyin_keeps_preedit_but_no_candidates() {
        // "xq" → 整串 segment 为空，找合法 head → "x" 也是单字音节 → tail="q"。
        // dict 中无 "xq..." 前缀 → 候选空，preedit 仍保留。
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "xq".chars() {
            e.key(k(c));
        }
        assert_eq!(e.preedit(), "xq");
        assert!(e.candidates().is_empty(), "xq should have no candidates");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    // --- M5: 翻页 + 页内选词 ---
    fn fixture_many_ni() -> (std::path::PathBuf, std::path::PathBuf) {
        let db = tmp_db("page");
        let yaml = std::env::temp_dir().join(format!("kime_page_{}.yaml", std::process::id()));
        std::fs::write(&yaml, "").unwrap();
        // raw INSERT 前必须建表：Dict::open 负责 schema，缺了 5 个 page_* 测试在 base 上即崩。
        let _dict = Dict::open(&db).unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        for i in 0..25 {
            conn.execute(
                "INSERT OR IGNORE INTO phrase(pinyin, text, freq, abbrev, user) VALUES (?, ?, ?, 'n', 0)",
                rusqlite::params![format!("ni'{}", i), format!("词{}", i), 1000 - i as i64],
            )
            .unwrap();
        }
        drop(conn);
        (db, yaml)
    }

    #[test]
    fn page_navigation_clamps_and_page_local_digits() {
        let (db, yaml) = fixture_many_ni();
        let dict = Dict::open(&db).unwrap();
        let mut e = Engine::new(dict, Config::default());
        for c in "ni".chars() {
            e.key(k(c));
        }
        assert!(e.candidates().len() >= 20);
        assert_eq!(e.page(), (0, 10));
        // 上一页越界钳位
        assert_eq!(e.key(code_k(KEY_MINUS)), Outcome::Consumed);
        assert_eq!(e.page(), (0, 10));
        // 下一页
        assert_eq!(e.key(code_k(KEY_EQUAL)), Outcome::Consumed);
        assert_eq!(e.page(), (1, 10));
        // 页内数字 2 → 全局第 12 个候选入 pending（#81）
        let out = e.key(k('2'));
        match out {
            Outcome::Consumed => assert!(e.preedit().contains("词11")),
            other => panic!("expected pending Consumed, got {:?}", other),
        }
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn page_digit_beyond_page_is_ignored() {
        let (db, yaml) = fixture_many_ni();
        let dict = Dict::open(&db).unwrap();
        let mut e = Engine::new(dict, Config::default());
        for c in "ni".chars() {
            e.key(k(c));
        }
        // 第 0 页只有 10 个候选：数字 9 有效，但先翻到最后一页（25 条 → 第 2 页只有 5 个）
        e.key(code_k(KEY_EQUAL));
        e.key(code_k(KEY_EQUAL));
        let last_page = e.page();
        assert_eq!(last_page, (2, 10));
        // 页内索引 8 超过最后页剩余 5 个 → Ignored
        assert_eq!(e.key(k('9')), Outcome::Ignored);
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn page_resets_on_commit_and_esc() {
        let (db, yaml) = fixture_many_ni();
        let dict = Dict::open(&db).unwrap();
        let mut e = Engine::new(dict, Config::default());
        for c in "ni".chars() {
            e.key(k(c));
        }
        e.key(code_k(KEY_EQUAL));
        assert_eq!(e.page(), (1, 10));
        e.key(code_k(KEY_ESC));
        assert_eq!(e.page(), (0, 10));
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn paging_without_candidates_is_ignored() {
        let (db, yaml) = fixture_many_ni();
        let dict = Dict::open(&db).unwrap();
        let mut e = Engine::new(dict, Config::default());
        assert_eq!(e.key(code_k(KEY_EQUAL)), Outcome::Ignored);
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    // --- M5: 模糊音 ---
    const FUZZY_YAML: &str =
        "---\n...\n里\tli\t800\n凉\tliang\t700\n饭\tfan\t900\n翻\tfang\t600\n你\tni\t5000\n";

    #[test]
    fn fuzzy_empty_config_is_baseline() {
        let db = tmp_db("fz0");
        let yaml = std::env::temp_dir().join("kime_fz0.yaml");
        std::fs::write(&yaml, FUZZY_YAML).unwrap();
        let mut dict = Dict::open(&db).unwrap();
        dict.import(&yaml).unwrap();
        let mut e = Engine::new(dict, Config::default());
        for c in "ni".chars() {
            e.key(k(c));
        }
        let texts: Vec<&str> = e.candidates().iter().map(|c| c.text.as_str()).collect();
        assert!(texts.contains(&"你"));
        assert!(!texts.contains(&"里"), "无模糊配置时不应命中 li 词");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn fuzzy_initial_n_to_l_matches_li_words() {
        let db = tmp_db("fz1");
        let yaml = std::env::temp_dir().join("kime_fz1.yaml");
        std::fs::write(&yaml, FUZZY_YAML).unwrap();
        let mut dict = Dict::open(&db).unwrap();
        dict.import(&yaml).unwrap();
        let cfg = Config {
            shuangpin: None,
            fuzzy: vec!["n=l".to_string()],
            ..Config::default()
        };
        let mut e = Engine::new(dict, cfg);
        for c in "ni".chars() {
            e.key(k(c));
        }
        let texts: Vec<&str> = e.candidates().iter().map(|c| c.text.as_str()).collect();
        assert!(texts.contains(&"你"));
        assert!(texts.contains(&"里"), "fuzzy n=l 应命中 li 词");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn fuzzy_final_an_to_ang_matches_fang() {
        let db = tmp_db("fz2");
        let yaml = std::env::temp_dir().join("kime_fz2.yaml");
        std::fs::write(&yaml, FUZZY_YAML).unwrap();
        let mut dict = Dict::open(&db).unwrap();
        dict.import(&yaml).unwrap();
        let cfg = Config {
            shuangpin: None,
            fuzzy: vec!["an=ang".to_string()],
            ..Config::default()
        };
        let mut e = Engine::new(dict, cfg);
        for c in "fan".chars() {
            e.key(k(c));
        }
        let texts: Vec<&str> = e.candidates().iter().map(|c| c.text.as_str()).collect();
        assert!(texts.contains(&"饭"));
        assert!(texts.contains(&"翻"), "fuzzy an=ang 应命中 fang 词");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn fuzzy_invalid_entries_ignored() {
        let db = tmp_db("fz3");
        let yaml = std::env::temp_dir().join("kime_fz3.yaml");
        std::fs::write(&yaml, FUZZY_YAML).unwrap();
        let mut dict = Dict::open(&db).unwrap();
        dict.import(&yaml).unwrap();
        let cfg = Config {
            shuangpin: None,
            fuzzy: vec!["xyz".to_string(), "=x".to_string(), "x=".to_string()],
            ..Config::default()
        };
        let mut e = Engine::new(dict, cfg);
        for c in "ni".chars() {
            e.key(k(c));
        }
        assert!(!e.candidates().is_empty(), "非法配置不影响正常查询");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn fuzzy_dedupe_across_paths() {
        let db = tmp_db("fz4");
        let yaml = std::env::temp_dir().join("kime_fz4.yaml");
        // 同一文本同时有 ni / li 两行（前缀互不包含）→ 主路与模糊路都命中但只出现一次
        std::fs::write(&yaml, "---\n...\n泥\tni\t600\n泥\tli\t500\n你\tni\t900\n").unwrap();
        let mut dict = Dict::open(&db).unwrap();
        dict.import(&yaml).unwrap();
        let cfg = Config {
            shuangpin: None,
            fuzzy: vec!["n=l".to_string()],
            ..Config::default()
        };
        let mut e = Engine::new(dict, cfg);
        for c in "ni".chars() {
            e.key(k(c));
        }
        let count = e.candidates().iter().filter(|c| c.text == "泥").count();
        assert_eq!(count, 1, "去重：同一文本只出现一次");
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn llm_merge_adds_candidates() {
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nihao".chars() {
            e.key(k(c));
        }
        let original_len = e.candidates().len();

        let ai = vec![Candidate {
            text: "你好世界".to_string(),
            pinyin: "ni'hao".to_string(),
            freq: 1,
            eff: 1,
            ai: true,
        }];
        e.merge_ai(ai);

        assert_eq!(e.candidates().len(), original_len + 1);
        assert!(e.candidates().iter().any(|c| c.text == "你好世界" && c.ai));
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn llm_dedupe_existing() {
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nihao".chars() {
            e.key(k(c));
        }
        let original_len = e.candidates().len();

        // Merge a candidate that already exists
        let ai = vec![Candidate {
            text: "你好".to_string(),
            pinyin: "ni'hao".to_string(),
            freq: 1,
            eff: 1,
            ai: true,
        }];
        e.merge_ai(ai);

        // Should not add duplicate
        assert_eq!(e.candidates().len(), original_len);
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn test_viterbi_sentence_first_candidate() {
        let db = tmp_db("viterbi_engine");
        let yaml = std::env::temp_dir().join("kime_viterbi_engine.yaml");
        std::fs::write(
            &yaml,
            "---\n...\n你好\tni hao\t5000\n世界\tshi jie\t4000\n你\tni\t1000\n好\thao\t1000\n",
        )
        .unwrap();
        let mut dict = Dict::open(&db).unwrap();
        dict.import(&yaml).unwrap();
        let cfg = Config {
            shuangpin: None,
            ..Config::default()
        };
        let mut e = Engine::new(dict, cfg);
        for c in "nihaoshijie".chars() {
            e.key(k(c));
        }
        let cands = e.candidates();
        assert!(!cands.is_empty());
        // 第一候选应当是由 Viterbi 最优路径合成的连贯整句「你好世界」
        assert_eq!(cands[0].text, "你好世界");

        // #81：空格选词入 pending（整句），拼音选完再按空格整段释放
        let outcome = e.key(Key {
            ch: None,
            code: KEY_SPACE,
            shift: false,
            ctrl: false,
            alt: false,
        });
        match outcome {
            Outcome::Consumed => {}
            other => panic!("expected pending Consumed, got {:?}", other),
        }
        assert_eq!(e.preedit(), "你好世界", "整句在 pending 显示");
        match e.key(code_k(KEY_SPACE)) {
            Outcome::Commit(text) => assert_eq!(text, "你好世界"),
            other => panic!("expected 释放 Commit, got {:?}", other),
        }
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn digit_0_selects_tenth_candidate() {
        // 构造 12 个候选，翻到第 1 页后按 0 → 选第 10 个（全局索引 9）
        let (db, yaml) = fixture_many_ni();
        let dict = Dict::open(&db).unwrap();
        let cfg = Config {
            page_size: 10,
            shuangpin: None,
            ..Config::default()
        };
        let mut e = Engine::new(dict, cfg);
        for c in "ni".chars() {
            e.key(k(c));
        }
        // 翻到第 2 页
        e.key(code_k(KEY_EQUAL));
        assert_eq!(e.page().0, 1);
        // 按 0 → 选第 10 个候选入 pending（#81；全局索引 1*10 + 9 = 19）
        let outcome = e.key(k('0'));
        match outcome {
            Outcome::Consumed => {}
            other => panic!("expected pending Consumed, got {:?}", other),
        }
        assert!(
            e.preedit().contains("词19"),
            "第 2 页第 10 个候选在 pending 显示串里（空格修页同路）",
        );
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    /// #81 空格修页：翻到第 2 页后按空格 = 选**当前页**首候选（全局
    /// 索引 10）入 pending，不再写死第一页首候选。
    #[test]
    fn space_selects_current_page_top_after_paging() {
        let (db, yaml) = fixture_many_ni();
        let dict = Dict::open(&db).unwrap();
        let cfg = Config {
            page_size: 10,
            shuangpin: None,
            ..Config::default()
        };
        let mut e = Engine::new(dict, cfg);
        for c in "ni".chars() {
            e.key(k(c));
        }
        e.key(code_k(KEY_EQUAL)); // 翻到第 2 页
        assert_eq!(e.page().0, 1);
        assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Consumed);
        assert!(
            e.preedit().contains("词10"),
            "第 2 页首候选（全局 10）在 pending 显示串里",
        );
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    /// #81 Esc 取消：选词入 pending 后按 Esc = 整句不上屏（应用零提交），
    /// pending 与剩余拼音全清；已释放词不受影响。
    #[test]
    fn esc_cancels_pending_without_commit() {
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nihaoni".chars() {
            e.key(k(c));
        }
        assert_eq!(
            e.key(code_k(KEY_SPACE)),
            Outcome::Consumed,
            "选词入 pending"
        );
        assert_eq!(e.preedit(), "你好ni");
        assert_eq!(e.key(code_k(KEY_ESC)), Outcome::Consumed, "Esc 取消");
        assert!(e.letters.is_empty());
        assert!(e.preedit().is_empty(), "显示串全清，无内容上屏");
        assert!(e.candidates().is_empty());
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn ctrl_dot_toggles_punct_mode() {
        let (mut e, db, yaml) = engine_with_fixture();
        // 默认中文标点模式
        assert!(matches!(e.punct_mode, crate::config::PunctMode::Chinese));
        // Ctrl+. 切换到英文
        let outcome = e.key(Key {
            ch: Some('.'),
            code: 0,
            shift: false,
            ctrl: true,
            alt: false,
        });
        assert_eq!(outcome, Outcome::Consumed);
        assert!(matches!(e.punct_mode, crate::config::PunctMode::English));
        // 再按回来
        let outcome = e.key(Key {
            ch: Some('.'),
            code: 0,
            shift: false,
            ctrl: true,
            alt: false,
        });
        assert_eq!(outcome, Outcome::Consumed);
        assert!(matches!(e.punct_mode, crate::config::PunctMode::Chinese));
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn english_punct_passes_through() {
        let (mut e, db, yaml) = engine_with_fixture();
        // 切换到英文标点模式
        e.punct_mode = crate::config::PunctMode::English;
        // 输入逗号 → 应原样输出 "," 而非 "，"
        let outcome = e.key(Key {
            ch: Some(','),
            code: 0,
            shift: false,
            ctrl: false,
            alt: false,
        });
        assert_eq!(outcome, Outcome::Commit(",".to_string()));
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }
}
