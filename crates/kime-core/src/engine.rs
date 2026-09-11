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
use crate::punct;
use kime_pinyin::segment;

// evdev keycodes — wayland 原生即此值，平台壳无需翻译。
const KEY_ESC: u32 = 1;
const KEY_BACKSPACE: u32 = 14;
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
    /// 最近一次查询得到的候选；commit / clear 时一并清空。
    candidates: Vec<Candidate>,
    /// 双拼解码表（如小鹤/自然码），用于 shuangpin 模式
    sp: Option<kime_shuangpin::Table>,
    /// 当前候选对应的读音序列，commit 时用于学习
    last_reading: Vec<String>,
    /// 缓存的 preedit 字符串（双拼模式为解码后拼音，全拼为 letters）
    preedit: String,
    /// 当前标点模式（中文全角 / 英文原样）
    punct_mode: crate::config::PunctMode,
}
impl Engine {
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
            candidates: Vec::new(),
            sp: shuangpin.map(kime_shuangpin::Table::new),
            last_reading: Vec::new(),
            preedit: String::new(),
            page_index: 0,
            fuzzy_map,
            punct_mode,
        }
    }
    /// 中/英文模式（英文模式所有键 Ignored 直通）
    pub fn chinese(&self) -> bool {
        self.chinese
    }

    /// 当前 preedit：未上屏拼音串（如 "niha"）
    pub fn preedit(&self) -> &str {
        &self.preedit
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

    /// 唯一入口。字母累积 / 退格删音节 / 数字选词 / 空格首选 / shift 中英切换
    pub fn key(&mut self, k: Key) -> Outcome {
        // Shift 单独按下：无组合时切中英；有预编辑时上屏原串再切英文。
        if k.ch.is_none()
            && k.shift
            && !k.ctrl
            && !k.alt
            && (k.code == KEY_LEFTSHIFT || k.code == KEY_RIGHTSHIFT)
        {
            if self.letters.is_empty() {
                self.chinese = !self.chinese;
                return Outcome::Consumed;
            }
            let text = self.letters.clone();
            self.letters.clear();
            self.candidates.clear();
            self.last_reading.clear();
            self.preedit.clear();
            self.page_index = 0;
            self.chinese = false;
            return Outcome::Commit(text);
        }

        // 英文模式：除上面已处理的 shift 外，其余键一律放行。
        if !self.chinese {
            return Outcome::Ignored;
        }

        // Backspace — evdev KEY_BACKSPACE（14），ch 通常为 None。
        if k.ch.is_none() && k.code == KEY_BACKSPACE {
            if self.letters.is_empty() {
                return Outcome::Ignored;
            }
            self.letters.pop();
            self.refresh_candidates();
            self.page_index = 0;
            return Outcome::Consumed;
        }

        // Esc — 清空当前组合。
        if k.ch.is_none() && k.code == KEY_ESC {
            self.letters.clear();
            self.candidates.clear();
            self.last_reading.clear();
            self.preedit.clear();
            self.page_index = 0;
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
                    let total_pages = self.candidates.len().div_ceil(self.page_size());
                    self.page_index = self.page_index.saturating_sub(1);
                    return Outcome::Consumed;
                }
            }
        }

        // Ctrl+f / Ctrl+b 翻页（Emacs 风格，无候选 Ignored，越界钳位）
        if k.ctrl && !k.alt && !k.shift && !self.candidates.is_empty() {
            let total_pages = self.candidates.len().div_ceil(self.page_size());
            match k.ch {
                Some('f') | Some('n') => {
                    self.page_index = (self.page_index + 1).min(total_pages - 1);
                    return Outcome::Consumed;
                }
                Some('b') | Some('p') => {
                    self.page_index = self.page_index.saturating_sub(1);
                    return Outcome::Consumed;
                }
                _ => {}
            }
        }

        // Space — 无候选时放行；有候选时上屏首选并清空。
        if k.ch.is_none() && k.code == KEY_SPACE {
            if let Some(top) = self.candidates.first().cloned() {
                let text = top.text.clone();
                self.learn_or_warn(&text);
                self.letters.clear();
                self.candidates.clear();
                self.last_reading.clear();
                self.preedit.clear();
                self.page_index = 0;
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
        // 标点处理：仅中文模式且有映射时生效
        if let Some(c) = k.ch {
            if let Some(mapped) = punct::map_punct(c) {
                // 英文标点模式：不转换，原样输出
                if self.punct_mode == crate::config::PunctMode::English {
                    return Outcome::Commit(c.to_string());
                }
                if self.letters.is_empty() {
                    // 情况 A：无预编辑串，直接上屏标点
                    return Outcome::Commit(mapped.to_string());
                } else {
                    // 有预编辑串，检查是否存在候选词
                    if let Some(top) = self.candidates.first().cloned() {
                        // 情况 B：顶字上屏，拼接标点
                        let text = top.text.clone();
                        let commit_text = format!("{}{}", text, mapped);
                        self.learn_or_warn(&text);
                        self.letters.clear();
                        self.candidates.clear();
                        self.last_reading.clear();
                        self.preedit.clear();
                        self.page_index = 0;
                        return Outcome::Commit(commit_text);
                    } else {
                        // 情况 C：无候选词，直接上屏并清空
                        let commit_text = format!("{}{}", self.letters, mapped);
                        self.letters.clear();
                        self.candidates.clear();
                        self.last_reading.clear();
                        self.preedit.clear();
                        self.page_index = 0;
                        return Outcome::Commit(commit_text);
                    }
                }
            }
        }
        if let Some(c) = k.ch {
            if c.is_ascii_alphabetic() {
                if k.alt || k.ctrl {
                    return Outcome::Ignored;
                }
                let lc = c.to_ascii_lowercase();
                self.letters.push(lc);
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
                        let text = cand.text.clone();
                        self.learn_or_warn(&text);
                        self.letters.clear();
                        self.candidates.clear();
                        self.last_reading.clear();
                        self.preedit.clear();
                        self.page_index = 0;
                        return Outcome::Commit(text);
                    }
                    return Outcome::Ignored;
                }
            }
        }

        // Enter（code 28）— 有预编辑串时原样上屏（不转中文），方便英文/网址
        if k.ch.is_none() && k.code == 28 {
            if !self.letters.is_empty() {
                let text = self.letters.clone();
                self.letters.clear();
                self.candidates.clear();
                self.last_reading.clear();
                self.preedit.clear();
                self.page_index = 0;
                return Outcome::Commit(text);
            }
            return Outcome::Ignored;
        }

        // 其它（标点、功能键等）→ 放行给宿主。
        Outcome::Ignored
    }
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

impl Engine {
    /// 上屏后记录用户选择。学习失败不影响本次上屏（文本已交给应用），
    /// 但必须可见：静默丢弃会让用户词永久不生效且无从排查。
    fn learn_or_warn(&mut self, text: &str) {
        if let Err(e) = self.dict.learn(&self.last_reading, text) {
            eprintln!("[kime] 用户词学习失败 ({} → {}): {}", self.preedit, text, e);
        }
    }

    /// 候选查询：双拼模式走 Table::to_syllables 解码，全拼模式走 kime_pinyin::segment + lookup_prefix，
    /// 兜底 lookup_abbrev。维护 self.last_reading 与 self.preedit。
    fn refresh_candidates(&mut self) {
        if self.letters.is_empty() {
            self.candidates.clear();
            self.last_reading.clear();
            self.preedit.clear();
            return;
        }

        // 双拼模式：解码失败时回退全拼切分（允许全拼混输，与 fcitx5 双拼行为一致）
        if let Some(ref table) = self.sp {
            let len = self.letters.len();
            let decoded = if len % 2 == 0 {
                table.to_syllables(&self.letters)
            } else {
                table.to_syllables(&self.letters[..len - 1])
            };
            match decoded {
                Ok(syllables) => {
                    let tail = if len % 2 == 0 {
                        ""
                    } else {
                        &self.letters[len - 1..]
                    };
                    self.last_reading = syllables.clone();
                    self.preedit = format!("{}{}", syllables.join(""), tail);
                    self.candidates = self
                        .dict
                        .lookup_prefix(&syllables, tail, self.config.candidate_limit)
                        .unwrap_or_default();
                    if self.candidates.is_empty() {
                        if let Ok(ab) = self
                            .dict
                            .lookup_abbrev(&self.letters, self.config.candidate_limit)
                        {
                            self.candidates = ab;
                        }
                    }
                    if self.candidates.is_empty() {
                        // 双拼解出错误音节（如 nihao→ni+ha）且查无词 → 全拼重试
                        self.refresh_full_pinyin();
                    }
                }
                Err(_) => {
                    // 非法键对：按全拼重新切分（preedit 保持原字母串）
                    self.refresh_full_pinyin();
                }
            }
            return;
        }

        self.refresh_full_pinyin();
    }

    /// 全拼路径：segment 切分 + 前缀查询 + 模糊音 + abbrev 兜底 + Viterbi 句级联想。
    fn refresh_full_pinyin(&mut self) {
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

        // 主路径查询
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

        // 若主路径与模糊路径都无候选，回退到缩写查询
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
                if let Some(sentence) = crate::lattice::viterbi_sentence(&self.dict, full_reading) {
                    if !cands.iter().any(|c| c.text == sentence.text) {
                        cands.insert(0, sentence);
                    }
                }
            }
        }

        self.candidates = cands;
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
        let mut conn = rusqlite::Connection::open(&db).unwrap();
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
        // 1 选词 "你好"，应触发 learn -> 候选顺序变化
        let outcome = e.key(k('1'));
        match outcome {
            Outcome::Commit(t) => assert_eq!(t, "你好"),
            other => panic!("expected Commit(你好), got {:?}", other),
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
        match e.key(code_k(KEY_SPACE)) {
            Outcome::Commit(t) => assert_eq!(t, "你好"),
            other => panic!("expected Commit, got {:?}", other),
        }
        assert!(e.preedit().is_empty());
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

    #[test]
    fn english_mode_disables_shuangpin() {
        let (mut e, db, yaml) = engine_with_shuangpin_fixture();
        e.key(shift_k(KEY_LEFTSHIFT)); // 切换到英文模式
                                       // 英文模式下所有按键 Ignored，所有状态清空
        assert!(!e.chinese());
        assert!(e.preedit().is_empty());
        assert!(e.candidates().is_empty());
        // 测试字母按键 Ignored
        assert_eq!(e.key(k('a')), Outcome::Ignored);
        // 测试数字按键 Ignored
        assert_eq!(e.key(k('1')), Outcome::Ignored);
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
            Outcome::Commit(t) => assert_eq!(t, "你好"),
            other => panic!("expected Commit(你好), got {:?}", other),
        }
        assert!(e.preedit().is_empty());
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
        match e.key(code_k(KEY_SPACE)) {
            Outcome::Commit(t) => assert_eq!(t, "你好"),
            other => panic!("expected Commit, got {:?}", other),
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

    #[test]
    fn esc_clears_composition() {
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nih".chars() {
            e.key(k(c));
        }
        assert_eq!(e.key(code_k(KEY_ESC)), Outcome::Consumed);
        assert!(e.preedit().is_empty());
        assert!(e.candidates().is_empty());
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn shift_toggles_to_english() {
        let (mut e, db, yaml) = engine_with_fixture();
        assert!(e.chinese());
        assert_eq!(e.key(shift_k(KEY_LEFTSHIFT)), Outcome::Consumed);
        assert!(!e.chinese());
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn english_mode_passes_keys_through() {
        let (mut e, db, yaml) = engine_with_fixture();
        e.key(shift_k(KEY_LEFTSHIFT)); // → english
        assert!(!e.chinese());
        assert_eq!(e.key(k('a')), Outcome::Ignored);
        assert!(e.preedit().is_empty());
        assert_eq!(e.key(code_k(KEY_SPACE)), Outcome::Ignored);
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn shift_with_active_composition_commits_raw_and_switches() {
        let (mut e, db, yaml) = engine_with_fixture();
        e.key(k('n'));
        let mut alt_n = k('n');
        alt_n.alt = true;
        assert_eq!(e.key(alt_n), Outcome::Ignored);
        assert_eq!(e.preedit(), "n");
        e.key(k('h'));
        assert!(e.chinese());
        assert_eq!(e.key(shift_k(KEY_LEFTSHIFT)), Outcome::Commit("nh".into()));
        assert!(!e.chinese());
        assert!(e.preedit().is_empty());
        assert_eq!(e.key(k('a')), Outcome::Ignored);
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
        let dict = Dict::open(&db).unwrap();
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
        // 页内数字 2 → 全局第 12 个候选
        let out = e.key(k('2'));
        match out {
            Outcome::Commit(t) => assert_eq!(t, "词11"),
            other => panic!("expected commit, got {:?}", other),
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
        assert!(e.candidates().len() > 0, "非法配置不影响正常查询");
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

        // 空格直接上屏整句
        let outcome = e.key(Key {
            ch: None,
            code: KEY_SPACE,
            shift: false,
            ctrl: false,
            alt: false,
        });
        match outcome {
            Outcome::Commit(text) => assert_eq!(text, "你好世界"),
            other => panic!("expected Commit, got {:?}", other),
        }
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn enter_commits_raw_preedit_without_conversion() {
        // 输入 "nihao" 后按 Enter → 应原样上屏 "nihao"（不转中文）
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nihao".chars() {
            e.key(k(c));
        }
        assert_eq!(e.preedit(), "nihao");
        let outcome = e.key(Key {
            ch: None,
            code: 28,
            shift: false,
            ctrl: false,
            alt: false,
        });
        assert_eq!(outcome, Outcome::Commit("nihao".to_string()));
        assert!(e.preedit().is_empty());
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
        // 按 0 → 选第 10 个候选（全局索引 1*10 + 9 = 19）
        let outcome = e.key(k('0'));
        match outcome {
            Outcome::Commit(text) => assert_eq!(text, "词19"),
            other => panic!("expected Commit(词19), got {:?}", other),
        }
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
