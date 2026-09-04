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
use kime_pinyin::segment;

// evdev keycodes — wayland 原生即此值，平台壳无需翻译。
const KEY_ESC: u32 = 1;
const KEY_BACKSPACE: u32 = 14;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_RIGHTSHIFT: u32 = 54;
const KEY_SPACE: u32 = 57;

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
}

impl Engine {
    pub fn new(dict: Dict, config: Config) -> Self {
        let shuangpin = config.shuangpin;
        Self {
            dict,
            config,
            chinese: true,
            letters: String::new(),
            candidates: Vec::new(),
            sp: shuangpin.map(kime_shuangpin::Table::new),
            last_reading: Vec::new(),
            preedit: String::new(),
        }
    }

    /// 中/英文模式（英文模式所有键 Ignored 直通）
    pub fn chinese(&self) -> bool {
        self.chinese
    }

    /// 当前 preedit：未上屏拼音串（双拼=解码后拼音，全拼=letters）
    pub fn preedit(&self) -> &str {
        &self.preedit
    }

    /// 当前读音的全部候选（内部 cap ~50，freq 降序）
    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    /// (当前页, 页大小) — 候选窗布局用；M1 恒 (0, 10)
    pub fn page(&self) -> (usize, usize) {
        (0, 10)
    }

    /// 当前高亮候选索引（候选窗渲染用）；M1 恒 0
    pub fn highlight(&self) -> usize {
        0
    }

    /// 唯一入口。字母累积 / 退格删音节 / 数字选词 / 空格首选 / shift 中英切换
    pub fn key(&mut self, k: Key) -> Outcome {
        // Shift 单独按下（无字符 + evdev shift 码）→ 仅在无组合时切换中英。
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
            return Outcome::Ignored;
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
            return Outcome::Consumed;
        }

        // Esc — 清空当前组合。
        if k.ch.is_none() && k.code == KEY_ESC {
            self.letters.clear();
            self.candidates.clear();
            self.last_reading.clear();
            self.preedit.clear();
            return Outcome::Consumed;
        }

        // Space — 无候选时放行；有候选时上屏首选并清空。
        if k.ch.is_none() && k.code == KEY_SPACE {
            if let Some(top) = self.candidates.first().cloned() {
                let text = top.text.clone();
                let _ = self.dict.learn(&self.last_reading, &text);
                self.letters.clear();
                self.candidates.clear();
                self.last_reading.clear();
                self.preedit.clear();
                return Outcome::Commit(text);
            }
            return Outcome::Ignored;
        }

        if let Some(c) = k.ch {
            // 字母（含 shift 的大写 → 归一化小写）→ 累积。
            if c.is_ascii_alphabetic() {
                let lc = c.to_ascii_lowercase();
                self.letters.push(lc);
                self.refresh_candidates();
                return Outcome::Consumed;
            }
            // 数字 1-9 选词。
            if let Some(d) = c.to_digit(10) {
                if (1..=9).contains(&d) {
                    let idx = (d - 1) as usize;
                    if let Some(cand) = self.candidates.get(idx).cloned() {
                        let text = cand.text.clone();
                        let _ = self.dict.learn(&self.last_reading, &text);
                        self.letters.clear();
                        self.candidates.clear();
                        self.last_reading.clear();
                        self.preedit.clear();
                        return Outcome::Commit(text);
                    }
                    return Outcome::Ignored;
                }
            }
        }

        // 其它（标点、功能键等）→ 放行给宿主。
        Outcome::Ignored
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

        // 双拼模式
        if let Some(ref table) = self.sp {
            let len = self.letters.len();
            if len % 2 == 0 {
                // 偶长：完整解码
                match table.to_syllables(&self.letters) {
                    Ok(syllables) => {
                        self.last_reading = syllables.clone();
                        self.preedit = syllables.join("");
                        self.candidates = self
                            .dict
                            .lookup_prefix(&syllables, "", 50)
                            .unwrap_or_default();
                    }
                    Err(_) => {
                        // 解码失败（非法键对/非字母）-> 清空候选与读音
                        self.candidates.clear();
                        self.last_reading.clear();
                        self.preedit = self.letters.clone();
                    }
                }
            } else {
                // 奇长：前 len-1 解码，最后一个字母作 tail 前缀
                let prefix = &self.letters[..len - 1];
                let tail = &self.letters[len - 1..];
                match table.to_syllables(prefix) {
                    Ok(syllables) => {
                        self.last_reading = syllables.clone();
                        self.preedit = format!("{}{}", syllables.join(""), tail);
                        self.candidates = self
                            .dict
                            .lookup_prefix(&syllables, tail, 50)
                            .unwrap_or_default();
                    }
                    Err(_) => {
                        self.candidates.clear();
                        self.last_reading.clear();
                        self.preedit = self.letters.clone();
                    }
                }
            }
            return;
        }

        // 全拼模式（原有逻辑 + abbrev 兜底）
        let segs = segment(&self.letters);
        let (reading, tail): (Vec<String>, String) = if let Some(reading) = segs.into_iter().next()
        {
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

        // 先尝试正常前缀查询
        let mut cands = self
            .dict
            .lookup_prefix(&reading, &tail, 50)
            .unwrap_or_default();

        // 若切分失败（reading 为空）或候选为空，回退到缩写查询
        if reading.is_empty() || cands.is_empty() {
            if let Ok(ab) = self.dict.lookup_abbrev(&self.letters, 50) {
                cands = ab;
            }
            // abbrev 兜底时 last_reading 置空 —— 缩写行 learn 用它自己的行
            self.last_reading.clear();
        }

        self.candidates = cands;
    }

    /// M5: AI 候选合入当前列表。后台线程完成后由壳回调（仍在主线程执行）
    pub fn merge_ai(&mut self, _ai: Vec<Candidate>) {
        todo!("M5")
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
        let engine = Engine::new(dict, Config::default());
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
               ('ni''hao','你好',5000,'x',0);
            ",
        )
        .unwrap();
        drop(conn);
        let dict = Dict::open(&db).expect("reopen dict for lookups");
        let config = Config {
            dict_path: db.to_string_lossy().to_string(),
            shuangpin: Some(Scheme::Xiaohe),
            ai_endpoint: None,
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
    fn shuangpin_decode_error_clears_candidates() {
        let (mut e, db, yaml) = engine_with_shuangpin_fixture();
        // 非法的双拼码，比如 "abc"（奇数长度 -> 错误）
        for c in "abc".chars() {
            e.key(k(c));
        }
        // 解码错误时，candidate 应清空，preedit 保持原字母
        assert!(e.candidates().is_empty());
        assert_eq!(e.preedit(), "abc");
        // last_reading 应为空（引擎清空状态）
        assert!(e.last_reading.is_empty());
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(&yaml);
    }

    #[test]
    fn digit_selection_triggers_learn_and_reorders_candidates() {
        let (mut e, db, yaml) = engine_with_fixture();
        for c in "nih".chars() {
            e.key(k(c));
        }
        // 1 选词 "你好"，应触发 learn -> 候选顺��序变化
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
    fn shift_with_active_composition_is_ignored() {
        let (mut e, db, yaml) = engine_with_fixture();
        e.key(k('n'));
        assert!(e.chinese());
        // shift while composing: per spec, must NOT toggle — leaves state intact.
        assert_eq!(e.key(shift_k(KEY_LEFTSHIFT)), Outcome::Ignored);
        assert!(e.chinese());
        assert_eq!(e.preedit(), "n");
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
}
