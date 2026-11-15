//! kime CLI 入口：吞 stdin 拼音串、出候选列表。
//!
//! 用法（M9 起）：
//!   kime status
//!   kime config <list|get KEY|set KEY VALUE>
//!   kime build-dict --in <sqlite> --out <bin>
//!   kime english [--dict <path>] <字母串>...
//!   kime debug [--scenario <commit-backspace|shift-mode|punct-idle-backspace>] [键 token ...]
//!   kime mine-lm [--dict <path>] [--jev] [--jev-endpoint <url>] [--jev-key <key>]
//!   kime --dict <path> [--import <yaml>]... [--import-english <yaml>]... [--shuangpin <scheme>]
//!   kime repl
//!
//! 配置字段: shuangpin(xiaohe|ziranma|none), page_size, candidate_limit
//!
//! `status` 与 `config` 已迁到 clap（见 [`Cli`]），其余子命令仍走
//! [`legacy_main`] 的手写解析。

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

use kime_core::builder::build;
use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::{Engine, Key, Outcome};
use kime_shuangpin::Scheme;

// evdev keycodes — 与 platform-wayland 壳同值
const KEY_ESC: u32 = 1;

/// 默认词库路径。
fn default_dict_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".local/share/kime/dict.sqlite3")
}

/// kime 输入法引擎：构建词库、查询候选、读写配置。
///
/// 迁移期说明（面向开发者，非用户帮助）：本阶段只建模 `status` 与 `config`
/// 两个子命令，其余仍走 [`legacy_main`] 的手写解析——未迁移的子命令与裸
/// flag（`--dict` / `--import` / `--shuangpin` / `repl` / `build-dict` /
/// `english` / `debug` / `mine-lm`）行为不变。
#[derive(Debug, Parser)]
#[command(name = "kime", version, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// 已迁移到 clap 的子命令。`None` = 裸 flag 模式，转 [`legacy_main`]。
#[derive(Debug, Subcommand)]
pub enum Command {
    /// 打印当前生效的模式与双拼方案
    Status,
    /// 读取或修改配置文件
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
}

/// `kime config` 的子命令。省略时等价于 `list`。
#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// 打印配置文件路径与全部内容
    List,
    /// 读取单个字段的当前值
    Get { key: ConfigKey },
    /// 写入单个字段并落盘
    Set { key: ConfigKey, value: String },
}

/// `kime config` 可读写的字段。键名非法由 clap 在解析期拒绝（旧实现是
/// 运行时 `未知字段` 报错）。
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ConfigKey {
    /// 双拼方案
    #[value(name = "shuangpin")]
    Shuangpin,
    /// 每页候选数
    #[value(name = "page_size")]
    PageSize,
    /// 单页候选上限
    #[value(name = "candidate_limit")]
    CandidateLimit,
    /// 词库路径（只读）
    #[value(name = "dict_path")]
    DictPath,
    /// 模糊音替换对（只读）
    #[value(name = "fuzzy")]
    Fuzzy,
    /// 标点模式
    #[value(name = "punct_mode")]
    PunctMode,
}

impl ConfigKey {
    /// 配置键的规范拼写（与 [`ConfigKey`] 的 `value(name)` 一致）。
    ///
    /// 下划线而非 clap 默认的 kebab-case：历史 CLI 一律用下划线，
    /// `candidate_limit` 不能被改写成 `candidate-limit`。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shuangpin => "shuangpin",
            Self::PageSize => "page_size",
            Self::CandidateLimit => "candidate_limit",
            Self::DictPath => "dict_path",
            Self::Fuzzy => "fuzzy",
            Self::PunctMode => "punct_mode",
        }
    }
}

/// `kime config set` 的取值：校验通过后得到的类型化字段集合。
///
/// 只有与 [`ConfigKey`] 对应的字段被填写，其余保持「当前配置值」，由
/// [`parse_config_value`] 保证不会串味。
#[derive(Debug, Clone)]
pub struct ConfigValue {
    /// 原始输入，供回显
    pub raw: String,
    /// `key = shuangpin` 时有效
    pub shuangpin: Option<Scheme>,
    /// `key = page_size` 时有效
    pub page_size: usize,
    /// `key = candidate_limit` 时有效
    pub candidate_limit: usize,
    /// `key = punct_mode` 时有效
    pub punct_mode: kime_core::config::PunctMode,
}

/// 按目标键校验取值并完成类型转换。
///
/// clap 无法在两个兄弟参数（key / value）间传递关联，故取值域的判定只能
/// 在拿到两者之后做。键名本身仍由 clap 的 `ValueEnum` 在解析期拒绝——
/// `kime config get nope` 不会再走到运行时。
fn parse_config_value(key: ConfigKey, raw: &str, config: &Config) -> Result<ConfigValue, String> {
    let mut value = ConfigValue {
        raw: raw.to_string(),
        shuangpin: config.shuangpin,
        page_size: config.page_size,
        candidate_limit: config.candidate_limit,
        punct_mode: config.punct_mode,
    };
    let invalid = |expect: &str| format!("无效值: {raw} (期望 {expect})");
    match key {
        ConfigKey::Shuangpin => {
            value.shuangpin = match raw {
                "xiaohe" => Some(Scheme::Xiaohe),
                "ziranma" => Some(Scheme::Ziranma),
                "none" => None,
                _ => return Err(invalid("xiaohe | ziranma | none")),
            };
        }
        ConfigKey::PageSize => {
            value.page_size = raw.parse().map_err(|_| invalid("正整数"))?;
        }
        ConfigKey::CandidateLimit => {
            value.candidate_limit = raw.parse().map_err(|_| invalid("正整数"))?;
        }
        ConfigKey::PunctMode => {
            value.punct_mode = match raw {
                "chinese" => kime_core::config::PunctMode::Chinese,
                "english" => kime_core::config::PunctMode::English,
                _ => return Err(invalid("chinese | english")),
            };
        }
        ConfigKey::DictPath | ConfigKey::Fuzzy => {
            return Err(format!("字段 {} 只读，不支持 set", key.as_str()))
        }
    }
    Ok(value)
}

/// `kime english hello` → 直查英文表，一行一个输入串。
///
/// 引擎还没接线英文路径（等编辑光标轨合并后由主控接），这条子命令是词库/查询本身的出口。
fn handle_english_cmd(args: &[String]) -> ExitCode {
    let mut dict_path: Option<PathBuf> = None;
    let mut words: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dict" => dict_path = it.next().map(PathBuf::from),
            other => words.push(other.to_string()),
        }
    }
    let path = dict_path.unwrap_or_else(default_dict_path);
    let dict = match Dict::open(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("failed to open dict {}: {e}", path.display());
            return ExitCode::from(1);
        }
    };
    for w in &words {
        let line = dict
            .lookup_english(w, 10)
            .iter()
            .map(|c| format!("{}({})", c.text, c.freq))
            .collect::<Vec<_>>()
            .join(" ");
        println!("{w}: {line}");
    }
    ExitCode::SUCCESS
}

/// evdev 码——与壳层同值。debug 子命令用。
const DBG_KEY_SPACE: u32 = 57;
const DBG_KEY_ENTER: u32 = 28;
const DBG_KEY_BACKSPACE: u32 = 14;
const DBG_KEY_DELETE: u32 = 11;
const DBG_KEY_SHIFT: u32 = 42;

fn dbg_key(code: u32, ch: Option<char>, ctrl: bool, alt: bool, shift: bool) -> Key {
    Key {
        ch,
        code,
        shift,
        ctrl,
        alt,
    }
}

/// 一个键 token → 引擎 Key。
/// 前缀 C-（Ctrl）/A-（Alt）；命名键 SHIFT/SPACE/ENTER/BACKSPACE/DELETE/ESC；
/// 其余当单个字符（字母/标点/数字）。SHIFT 走 evdev 42 + shift 位，
/// 命中引擎 Shift 分支（轻点切中/英 / 有组合冲刷切英）。
fn dbg_token_to_key(tok: &str) -> Key {
    let mut ctrl = false;
    let mut alt = false;
    let mut rest = tok;
    if let Some(r) = rest.strip_prefix("C-") {
        ctrl = true;
        rest = r;
    }
    if let Some(r) = rest.strip_prefix("A-") {
        alt = true;
        rest = r;
    }
    match rest {
        "SHIFT" => dbg_key(DBG_KEY_SHIFT, None, false, false, true),
        "SPACE" => dbg_key(DBG_KEY_SPACE, None, ctrl, alt, false),
        "ENTER" | "RET" => dbg_key(DBG_KEY_ENTER, None, ctrl, alt, false),
        "BACKSPACE" | "BSP" => dbg_key(DBG_KEY_BACKSPACE, None, ctrl, alt, false),
        "DELETE" | "DEL" => dbg_key(DBG_KEY_DELETE, None, ctrl, alt, false),
        "ESC" => dbg_key(KEY_ESC, None, ctrl, alt, false),
        _ => {
            let c = rest.chars().next().unwrap_or('a');
            dbg_key(0, Some(c), ctrl, alt, false)
        }
    }
}

/// 内置场景（不依赖用户词库）：上屏后退格 / Shift 切英文 / 标点直出。
fn dbg_builtin_scenario(name: &str) -> Vec<String> {
    match name {
        "commit-backspace" => {
            // 打 nihao 上屏「你好」→ 连按两次退格：修复后应为 Ignored（应用删字），
            // 不再把拼音弹回组合、不再弹候选面板。
            vec!["n", "i", "h", "a", "o", "SPACE", "BACKSPACE", "BACKSPACE"]
                .into_iter()
                .map(String::from)
                .collect()
        }
        "shift-mode" => {
            // 轻点 Shift 切英文（a/b/c 直通）→ 再轻点切回中文（nihao 重新组拼音）→ 上屏。
            vec![
                "SHIFT", "a", "b", "c", "SHIFT", "n", "i", "h", "a", "o", "SPACE",
            ]
            .into_iter()
            .map(String::from)
            .collect()
        }
        "punct-idle-backspace" => {
            // 中文模式下打「。」（. 转换上屏）→ 退格 = Ignored 放行应用删全角字符。
            vec![".", "BACKSPACE", "BACKSPACE"]
                .into_iter()
                .map(String::from)
                .collect()
        }
        other => {
            eprintln!(
                "unknown scenario '{other}': use commit-backspace | shift-mode | punct-idle-backspace"
            );
            Vec::new()
        }
    }
}

/// `kime debug` —— 无头面板调试：喂键序列驱动 Engine，逐键打印
/// 候选/preedit/模式/撤销深度，不启动 IME server，不碰桌面与 fcitx5。
///
/// 用法：
///   kime debug --scenario <commit-backspace|shift-mode|punct-idle-backspace>
///   kime debug tok1 tok2 ...            （自定义键序列）
///   cat keys.txt | kime debug           （每 token 一个，空白分隔）
///   kime debug --dict <库> --full r u   （打印全部候选，不截断到 6 条）
///   kime debug --dict <库> --json r u   （每键一行 JSON，供 agent/脚本消费）
///
/// `--full` 与 `--json` 是排查排序问题的主力：默认只打印前 6 条，看不出
/// 「中文候选是否整体在英文之前」这类跨块结论；JSON 让自动化脚本按字段断言，
/// 不必解析给人看的排版。
fn handle_debug_cmd(args: &[String]) -> ExitCode {
    let mut dict_path: Option<PathBuf> = None;
    let mut scenario: Option<String> = None;
    let mut corpus: Option<PathBuf> = None;
    let mut tokens: Vec<String> = Vec::new();
    let mut full = false;
    let mut json = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dict" => dict_path = it.next().map(PathBuf::from),
            "--scenario" => scenario = it.next().cloned(),
            "--corpus" => corpus = it.next().map(PathBuf::from),
            "--full" => full = true,
            "--json" => json = true,
            other => tokens.push(other.to_string()),
        }
    }
    // 语料模式：按文件里的 (mode, keys, expect) 逐条回放，独立于 token 序列。
    // 存在的理由：排查排序问题时「改前跑一次、改后跑一次」必须是一条命令，
    // 而不是让 agent 手写按键序列再自己比对输出。
    if let Some(path) = corpus {
        return run_corpus(&path, dict_path.as_deref());
    }
    if let Some(name) = &scenario {
        tokens = dbg_builtin_scenario(name);
    }
    if tokens.is_empty() {
        eprintln!("no tokens: pass key tokens or --scenario/--corpus; 见 --help");
        return ExitCode::from(2);
    }

    // 词库：指定则用；否则临时词库（可被选词 learn 改动，不落用户库）
    let dict = match dict_path {
        Some(p) => match Dict::open(&p) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("failed to open dict {}: {e}", p.display());
                return ExitCode::from(1);
            }
        },
        None => {
            let dir = std::env::temp_dir().join(format!(
                "kime_debug_{}_{}",
                std::process::id(),
                // 系统时钟早于 UNIX_EPOCH 时 duration_since 返回 Err；此处只需
                // 一个不重复的临时目录名，退化取 0 即可，不值得 panic。
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&dir).ok();
            let db = dir.join("dict.sqlite3");
            let mut d = match Dict::open(&db) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("failed to open temp dict {}: {e}", db.display());
                    return ExitCode::from(1);
                }
            };
            // 固定 fixture 词：nihao→你好 首候选，保证场景可复现
            let yml = dir.join("fx.yaml");
            std::fs::write(
                &yml,
                "...\n你好\tni hao\t5000\n泥猴\tni hou\t100\n世界\tshi jie\t9999\n安\ta\t8000\n",
            )
            .ok();
            match d.import(&yml) {
                Ok(n) => eprintln!("fixture dict: {} rows @ {}", n, db.display()),
                Err(e) => {
                    eprintln!("fixture import failed: {e}");
                    return ExitCode::from(1);
                }
            }
            d
        }
    };

    let mut engine = Engine::new(dict, Config::load());
    eprintln!(
        "debug: {} keys, chinese={} | mode 中=Chinese 英=English | cands=当前页候选",
        tokens.len(),
        engine.chinese()
    );
    for (i, tok) in tokens.iter().enumerate() {
        let outcome = engine.key(dbg_token_to_key(tok));
        let cands: Vec<String> = engine.candidates().iter().map(|c| c.text.clone()).collect();
        if json {
            // 机器可读：一行一键，字段固定。agent/脚本按 cands 数组断言相对顺序，
            // 不依赖人读排版。lang/layer 一起输出——排序问题的结论往往是
            // 「某个英文词排到了中文候选中间」，只有文本看不出来，带层与语言才看得见。
            // lang/layer 走 {:?}（枚举 Debug 输出是合法 JSON 字符串字面量：Zh/Exact），
            // outcome 是结构体变体，必须显式 json_str 包一层——否则输出
            // "outcome":Consumed 不是合法 JSON，脚本解析直接失败。
            let items: Vec<String> = engine
                .candidates()
                .iter()
                .map(|c| {
                    format!(
                        "{{\"text\":{},\"lang\":{},\"layer\":{},\"eff\":{}}}",
                        json_str(&c.text),
                        json_str(&format!("{:?}", c.lang)),
                        json_str(&format!("{:?}", c.layer)),
                        c.eff
                    )
                })
                .collect();
            println!(
                "{{\"i\":{},\"tok\":{},\"outcome\":{},\"mode\":{},\"preedit\":{},\"cands\":[{}]}}",
                i + 1,
                json_str(tok),
                json_str(&format!("{outcome:?}")),
                json_str(if engine.chinese() { "zh" } else { "en" }),
                json_str(engine.preedit()),
                items.join(",")
            );
            continue;
        }
        let shown = if full {
            cands.len()
        } else {
            cands.len().min(6)
        };
        let preview = cands
            .iter()
            .take(shown)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "[{:>2}] {:<10} => {:?} | mode={} preedit={:?} cands[{}]= {} undo={}",
            i + 1,
            tok,
            outcome,
            if engine.chinese() { "中" } else { "英" },
            engine.preedit(),
            cands.len(),
            preview,
            engine.undo_depth()
        );
    }
    ExitCode::SUCCESS
}

/// 最小 JSON 字符串转义（候选文本可能含引号/反斜杠/控制字符）。
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// 语料回放：按文件里的 (mode, keys, expect) 逐条喂键、校验断言。
///
/// 引擎按 mode 缓存（同一 mode 复用，每条用例前用 ESC 清空组合），因为开一次
/// 真实词库要几百毫秒——每条用例都重开会让语料跑不起来。用例之间不共享学习状态：
/// 语料里没有上屏键，ESC 只清组合不写任何表。
///
/// 失败时打印实际候选表——排查时最需要的信息就是「实际顺序是什么」。任一用例
/// 失败 → 退出码 1，因此可以直接当 agent/CI 的验收命令。
fn run_corpus(path: &Path, dict_path: Option<&Path>) -> ExitCode {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("failed to read corpus {}: {e}", path.display());
            return ExitCode::from(1);
        }
    };
    // 每种 mode 一个引擎（懒建）；dict_path 为 None 时用内存库，只适合解析自检。
    let mut engines: Vec<(Option<Scheme>, Engine)> = Vec::new();
    let mut failures = 0usize;
    let mut total = 0usize;

    for (i, raw) in text.lines().enumerate() {
        let row = raw.trim();
        if row.is_empty() || row.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = row.split('\t').collect();
        if cols.len() != 3 {
            eprintln!("corpus 第 {} 行必须正好 3 列：{row:?}", i + 1);
            failures += 1;
            continue;
        }
        let Some(shuangpin) = dbg_parse_mode(cols[0]) else {
            eprintln!("corpus 第 {} 行：未知模式 {:?}", i + 1, cols[0]);
            failures += 1;
            continue;
        };
        total += 1;

        let slot = match engines.iter().position(|(m, _)| *m == shuangpin) {
            Some(idx) => idx,
            None => {
                let dict = match dbg_open_dict(dict_path) {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("{e}");
                        return ExitCode::from(1);
                    }
                };
                let mut config = Config::load();
                config.shuangpin = shuangpin;
                engines.push((shuangpin, Engine::new(dict, config)));
                engines.len() - 1
            }
        };
        let engine = &mut engines[slot].1;
        // 用例之间清空组合：ESC 只清当前输入，不落任何学习记录。
        // 循环条件看 preedit 而不是 undo_depth——undo_depth 统计的是撤销栈，
        // 组合非空时它仍可能是 0，用它当次数会一次都不清（实测串味）。
        for _ in 0..8 {
            if engine.preedit().is_empty() && engine.candidates().is_empty() {
                break;
            }
            engine.key(dbg_token_to_key("ESC"));
        }
        let cands = dbg_feed(engine, cols[1]);
        match dbg_check(cols[2], &cands) {
            Ok(()) => println!(
                "PASS  {:<22} keys={:<10} cands={}",
                cols[0],
                cols[1],
                cands.len()
            ),
            Err(e) => {
                failures += 1;
                let shown: Vec<&str> = cands.iter().take(24).map(String::as_str).collect();
                println!(
                    "FAIL  {:<22} keys={:<10} {e}\n      实际候选[{}] = {:?}",
                    cols[0],
                    cols[1],
                    cands.len(),
                    shown
                );
            }
        }
    }
    if total == 0 {
        eprintln!("corpus 里没有可执行用例：{}", path.display());
        return ExitCode::from(2);
    }
    println!("corpus: {total} 条，失败 {failures} 条");
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// 打开词库：给定路径则开它，否则开一个内存库（语料只做解析自检时用）。
fn dbg_open_dict(dict_path: Option<&Path>) -> Result<Dict, String> {
    match dict_path {
        Some(p) => Dict::open(p).map_err(|e| format!("failed to open dict {}: {e}", p.display())),
        None => Dict::open(":memory:").map_err(|e| format!("failed to open temp dict: {e}")),
    }
}

/// 语料模式列 → 双拼方案（`None` = 全拼）。
fn dbg_parse_mode(raw: &str) -> Option<Option<Scheme>> {
    match raw {
        "full" => Some(None),
        other => other
            .strip_prefix("shuangpin:")
            .and_then(|name| match name {
                "ziranma" => Some(Scheme::Ziranma),
                "xiaohe" => Some(Scheme::Xiaohe),
                _ => None,
            })
            .map(Some),
    }
}

/// 喂一串字符键，返回最后一次按键后的候选文本。
fn dbg_feed(engine: &mut Engine, keys: &str) -> Vec<String> {
    let mut last = Vec::new();
    for c in keys.chars() {
        engine.key(dbg_token_to_key(&c.to_string()));
        last = engine.candidates().iter().map(|c| c.text.clone()).collect();
    }
    last
}

/// 校验一条断言（语法与 `debug_corpus_test.rs` 一致）。
fn dbg_check(expect: &str, cands: &[String]) -> Result<(), String> {
    let pos = |t: &str| cands.iter().position(|c| c == t);
    for raw in expect.split(';').filter(|s| !s.trim().is_empty()) {
        if let Some(v) = raw.strip_prefix("first=") {
            if cands.first().map(String::as_str) != Some(v) {
                return Err(format!("首候选应为 {v:?}，实际 {:?}", cands.first()));
            }
        } else if let Some(v) = raw.strip_prefix("has=") {
            if pos(v).is_none() {
                return Err(format!("候选里缺少 {v:?}"));
            }
        } else if let Some(v) = raw.strip_prefix("absent=") {
            if pos(v).is_some() {
                return Err(format!("候选里不该出现 {v:?}"));
            }
        } else if let Some((l, r)) = raw.split_once('<') {
            match (pos(l), pos(r)) {
                (Some(a), Some(b)) if a < b => {}
                (a, b) => return Err(format!("{l:?} 必须在 {r:?} 之前，实际 左={a:?} 右={b:?}")),
            }
        } else {
            return Err(format!("无法解析断言 {raw:?}"));
        }
    }
    Ok(())
}

/// 配置文件路径：`$KIME_CONFIG_PATH` 优先，否则 `~/.config/kime/config.toml`。
fn resolve_config_path() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("KIME_CONFIG_PATH") {
        return Ok(PathBuf::from(p));
    }
    match std::env::var("HOME") {
        Ok(home) => Ok(PathBuf::from(home)
            .join(".config")
            .join("kime")
            .join("config.toml")),
        Err(_) => Err("HOME 未设置".to_string()),
    }
}

/// `kime config <list|get KEY|set KEY VALUE>` —— 读配置与写配置。
///
/// 键名由 clap 的 `ValueEnum` 在解析期校验，取值域由 [`parse_config_value`]
/// 按目标键判定；此处只做落盘。
fn handle_config_cmd(action: Option<ConfigAction>) -> Result<String, String> {
    let config_path = resolve_config_path()?;
    let mut config = Config::load_from_path(&config_path);
    match action.unwrap_or(ConfigAction::List) {
        ConfigAction::List => {
            let toml_str =
                toml::to_string_pretty(&config).map_err(|e| format!("序列化失败: {}", e))?;
            Ok(format!("{}\n\n{}", config_path.display(), toml_str))
        }
        ConfigAction::Get { key } => Ok(match &key {
            ConfigKey::Shuangpin => format!("{:?}", config.shuangpin),
            ConfigKey::PageSize => config.page_size.to_string(),
            ConfigKey::CandidateLimit => config.candidate_limit.to_string(),
            ConfigKey::DictPath => config.dict_path.clone(),
            ConfigKey::Fuzzy => format!("{:?}", config.fuzzy),
            ConfigKey::PunctMode => format!("{:?}", config.punct_mode),
        }),
        ConfigAction::Set { key, value } => {
            let value = parse_config_value(key, &value, &config)?;
            match key {
                ConfigKey::Shuangpin => config.shuangpin = value.shuangpin,
                ConfigKey::PageSize => config.page_size = value.page_size,
                ConfigKey::CandidateLimit => config.candidate_limit = value.candidate_limit,
                ConfigKey::PunctMode => config.punct_mode = value.punct_mode,
                // 只读字段在 parse_config_value 内已提前返回。
                ConfigKey::DictPath | ConfigKey::Fuzzy => unreachable!(),
            }
            if let Some(parent) = config_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let toml_str =
                toml::to_string_pretty(&config).map_err(|e| format!("序列化失败: {}", e))?;
            std::fs::write(&config_path, toml_str).map_err(|e| format!("写入失败: {}", e))?;
            Ok(format!("已更新 {} = {}", key.as_str(), value.raw))
        }
    }
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    // 迁移期闸门：只有已迁移的子命令交给 clap，其余（含裸 flag 与 REPL）
    // 仍走 legacy 解析，行为不变。下一步把 `english` / `debug` /
    // `mine-lm` / `build-dict` 加进这个 match，最后连 legacy 一并删掉。
    //
    // `--help` / `--version` 一并交给 clap：它们描述的是顶层命令面，
    // 由 legacy 那份手写字符串继续答会与已迁移子命令脱节（`status`
    // 就不在 legacy help 里）。legacy 的 `--help` 分支随最后一批迁移删除。
    if argv.first().is_some_and(|head| {
        matches!(
            head.as_str(),
            "status" | "config" | "--help" | "-h" | "--version" | "-V"
        )
    }) {
        return match Cli::parse().command {
            Some(command) => run_migrated(command),
            // 闸门已保证 command 必为 Some；留作 clap 行为变化时的兜底。
            None => ExitCode::from(2),
        };
    }
    legacy_main(&argv)
}

/// 已迁移子命令的执行体。
fn run_migrated(command: Command) -> ExitCode {
    match command {
        Command::Status => {
            let config = Config::load();
            let mode = if config.punct_mode == kime_core::config::PunctMode::Chinese {
                "chinese"
            } else {
                "english"
            };
            let scheme = format!("{:?}", config.shuangpin);
            println!("mode: {}, scheme: {}", mode, scheme);
            ExitCode::SUCCESS
        }
        Command::Config { action } => match handle_config_cmd(action) {
            Ok(s) => {
                println!("{}", s);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("config error: {}", e);
                ExitCode::from(2)
            }
        },
    }
}

/// 迁移期 legacy 解析：未迁移子命令 + 裸 flag + REPL。行为与迁移前逐字一致。
fn legacy_main(argv: &[String]) -> ExitCode {
    let mut args_iter = argv.iter().peekable();
    let first = args_iter.next();

    // 子命令分派
    match first.map(|s| s.as_str()) {
        Some("english") => {
            let sub_args: Vec<String> = args_iter.map(|s| s.to_string()).collect();
            return handle_english_cmd(&sub_args);
        }
        Some("debug") => {
            let sub_args: Vec<String> = args_iter.map(|s| s.to_string()).collect();
            return handle_debug_cmd(&sub_args);
        }
        Some("typing") => {
            let sub_args: Vec<String> = args_iter.map(|s| s.to_string()).collect();
            return handle_typing_cmd(&sub_args);
        }
        Some("mine-lm") => {
            let mut dict_path: Option<PathBuf> = None;
            let mut jev_enabled = false;
            let mut jev_endpoint: Option<String> = None;
            let mut jev_key: Option<String> = None;
            let mut sub_args = args_iter.peekable();
            while let Some(arg) = sub_args.next() {
                match arg.as_str() {
                    "--dict" => dict_path = sub_args.next().map(PathBuf::from),
                    // jev 语义门控（#88）：准入候选先过 jev 判定再学库；
                    // 端点 = CLI 覆盖 > config.jev_endpoint；key = CLI > KIME_JEV_KEY
                    // 环境变量（凭据不落 config/词库/任何提交文件）
                    "--jev" => jev_enabled = true,
                    "--jev-endpoint" => jev_endpoint = sub_args.next().cloned(),
                    "--jev-key" => jev_key = sub_args.next().cloned(),
                    _ => {}
                }
            }
            let path = dict_path.unwrap_or_else(default_dict_path);
            let gate: Option<kime_core::lm::Jeving> = if jev_enabled {
                let config = Config::load();
                let endpoint = match jev_endpoint.or(config.jev_endpoint) {
                    Some(endpoint) => endpoint,
                    None => {
                        eprintln!(
                            "mine-lm: --jev 需要端点（config jev_endpoint 或 --jev-endpoint）"
                        );
                        return ExitCode::from(2);
                    }
                };
                let key = jev_key.or_else(|| std::env::var("KIME_JEV_KEY").ok());
                Some(kime_core::lm::Jeving::new(endpoint, config.jev_model, key))
            } else {
                None
            };
            let start = std::time::Instant::now();
            return match Dict::open(&path) {
                Ok(d) => match kime_core::lm::mine_gated(d.conn(), gate.as_ref()) {
                    Ok(st) => {
                        eprintln!(
                            "mined: log {} rows, admitted {} pairs, tail_mined {} pairs, evicted {}, purged {}, gen {} in {:?}",
                            st.log_rows, st.admitted, st.tail_mined, st.evicted, st.purged_rows,
                            st.generation, start.elapsed()
                        );
                        // 无 --jev 时不打印这行：无门控路径输出与合入前一致
                        if gate.is_some() {
                            eprintln!(
                                "jev gate: rejected {} pairs, fallback {} pairs",
                                st.jev_gated, st.jev_skipped
                            );
                        }
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("mine-lm error: {e}");
                        ExitCode::from(2)
                    }
                },
                Err(e) => {
                    eprintln!("mine-lm: 无法打开词库 {}: {e}", path.display());
                    ExitCode::from(2)
                }
            };
        }
        _ => {}
    }

    let mut dict_path: Option<PathBuf> = None;
    // `--import` 可重复：rime-ice 是 5~6 张分表，一次调用要全部喂进去。
    // 旧实现用单个 Option 存路径，第二个 `--import` 会静默覆盖第一个 → 只导了一张表。
    let mut import_paths: Vec<PathBuf> = Vec::new();
    // rime-ice 的英文表走单独入口：`Dict::import` 灌 `phrase`（拼音索引），
    // `Dict::import_english` 灌 `english`（原始按键串索引）。喂错表会污染中文候选。
    let mut import_english_paths: Vec<PathBuf> = Vec::new();
    let mut shuangpin: Option<Option<Scheme>> = None;
    let mut args_iter = argv.iter().peekable();

    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "--dict" => {
                dict_path = args_iter.next().map(PathBuf::from);
            }
            "--import" => {
                if let Some(p) = args_iter.next().map(PathBuf::from) {
                    import_paths.push(p);
                }
            }
            "--import-english" => {
                if let Some(p) = args_iter.next().map(PathBuf::from) {
                    import_english_paths.push(p);
                }
            }
            "--shuangpin" => {
                let s = args_iter.next().cloned().unwrap_or_default();
                shuangpin = Some(match s.as_str() {
                    "xiaohe" => Some(Scheme::Xiaohe),
                    "ziranma" => Some(Scheme::Ziranma),
                    "none" => None,
                    other => {
                        eprintln!("unknown shuangpin scheme: {other}");
                        return ExitCode::from(2);
                    }
                });
            }
            "repl" => {}
            "build-dict" => {
                let mut in_path = None;
                let mut out_path = None;
                while let Some(sub_arg) = args_iter.next() {
                    match sub_arg.as_str() {
                        "--in" => in_path = args_iter.next().map(PathBuf::from),
                        "--out" => out_path = args_iter.next().map(PathBuf::from),
                        _ => {}
                    }
                }
                let (inp, outp) = match (in_path, out_path) {
                    (Some(i), Some(o)) => (i, o),
                    _ => {
                        eprintln!("build-dict requires --in <sqlite> --out <bin>");
                        return ExitCode::from(2);
                    }
                };
                let start = std::time::Instant::now();
                match build(&inp, &outp) {
                    Ok(count) => {
                        let ok_size = std::fs::metadata(&outp).map(|m| m.len()).unwrap_or(0);
                        eprintln!(
                            "built {} entries → {} bytes in {:?}",
                            count,
                            ok_size,
                            start.elapsed()
                        );
                        return ExitCode::SUCCESS;
                    }
                    Err(e) => {
                        eprintln!("failed: {e}");
                        return ExitCode::from(1);
                    }
                }
            }
            other => {
                eprintln!("unknown arg: {other}");
                return ExitCode::from(2);
            }
        }
    }

    // REPL
    let path = dict_path.unwrap_or_else(default_dict_path);
    let mut dict = match Dict::open(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "failed to open dict {}: {e}\ntry: kime build-dict --in ... --out ~/.local/share/kime/dict.bin",
                path.display()
            );
            return ExitCode::from(1);
        }
    };
    // 逐表导入，顺序随便：`Dict::import` 冲突时取频率较大者。
    // 旧实现是 `INSERT OR IGNORE` + 按文件名 glob，无频率列的 `cn_dicts/41448`（46,031 条，
    // 频率全空）字典序排在带真实频率的 `cn_dicts/8105`（8,783 条）之前，先落库的空 0 值
    // 把 8105 的频率全 IGNORE 掉了 —— 这才是「打 shi 出不来『是』」的根因。
    // 现在 41448 导不导都不影响已有条目的频率，只决定要不要那批生僻字（rime-ice 自己
    // 把它注释在「按需启用」）；本次重建沿用了库里已有的那批，一个字符都没丢。
    for yaml in &import_paths {
        match dict.import(yaml) {
            Ok(n) => eprintln!("imported {}: {n} rows", yaml.display()),
            Err(e) => {
                eprintln!("import failed for {}: {e}", yaml.display());
                return ExitCode::from(1);
            }
        }
    }
    // 英文表同理：顺序随便，同 `text` 重复时频率取大，幂等。
    for yaml in &import_english_paths {
        match dict.import_english(yaml) {
            Ok(n) => eprintln!("imported english {}: {n} rows", yaml.display()),
            Err(e) => {
                eprintln!("english import failed for {}: {e}", yaml.display());
                return ExitCode::from(1);
            }
        }
    }
    // CLI --shuangpin 覆盖配置文件里的方案
    let mut config = Config::load();
    if let Some(sp) = shuangpin {
        config.shuangpin = sp;
    }
    let mut engine = Engine::new(dict, config);

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[kime] 读取标准输入失败: {e}");
                return ExitCode::from(1);
            }
        };
        let input = line.trim().to_string();
        if input.is_empty() {
            continue;
        }
        // 每行视为独立输入：先 ESC 清掉上一行遗留的组合状态。
        let esc = Key {
            ch: None,
            code: KEY_ESC,
            shift: false,
            ctrl: false,
            alt: false,
        };
        engine.key(esc);
        let mut outcome = None;
        for c in input.chars() {
            // 字母走拼音累积，ASCII 标点走顶字上屏；其余（数字/中文/控制符）丢弃
            if !c.is_ascii_alphabetic() && !c.is_ascii_punctuation() {
                continue;
            }
            let key = Key {
                ch: Some(c),
                code: 0,
                shift: false,
                ctrl: false,
                alt: false,
            };
            outcome = Some(engine.key(key));
        }
        let outcome = match outcome {
            Some(o) => o,
            None => {
                let _ = writeln!(stdout, "{}: (非法输入)", input);
                continue;
            }
        };
        // 已上屏时候选必然为空，不该再报「无候选」
        if let Outcome::Commit(text) = outcome {
            let _ = writeln!(stdout, "=> {text}");
            continue;
        }
        let cands = engine.candidates();
        let preedit = engine.preedit();
        if cands.is_empty() {
            let _ = writeln!(stdout, "{}: (无候选)", input);
        } else {
            let list: Vec<String> = cands
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{}.{text}", i + 1, text = c.text))
                .collect();
            let _ = writeln!(stdout, "{preedit}: {}", list.join(" "));
        }
    }
    ExitCode::SUCCESS
}

/// `kime typing <文本文件>` —— 打字语料回放：逐词统计候选名次并落一份 markdown 报告。
///
///   kime typing <file> [--dict <库>] [--out <报告.md>] [--limit N] [--json]
///                   [--mode full|ziranma|xiaohe]
///
/// 为什么单独做一个命令：候选顺序的回归测试用的是短串 fixture，而用户抱怨的是
/// 「打一整段话时顺序不对」。这个命令把整段文本当成一次连续输入跑，产出可持久化
/// 的数字，让改前/改后能对照。
fn handle_typing_cmd(args: &[String]) -> ExitCode {
    let mut dict_path: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut limit: usize = 40;
    let mut json = false;
    let mut mode: Option<Option<kime_core::Scheme>> = Some(None); // 默认全拼
    let mut inputs: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dict" => dict_path = it.next().map(PathBuf::from),
            "--out" => out = it.next().map(PathBuf::from),
            "--limit" => limit = it.next().and_then(|s| s.parse().ok()).unwrap_or(40),
            "--json" => json = true,
            "--mode" => {
                mode = it.next().and_then(|m| match m.as_str() {
                    "full" => Some(None),
                    "ziranma" => Some(Some(kime_core::Scheme::Ziranma)),
                    "xiaohe" => Some(Some(kime_core::Scheme::Xiaohe)),
                    _ => None,
                });
                if mode.is_none() {
                    eprintln!("--mode 只支持 full | ziranma | xiaohe");
                    return ExitCode::from(2);
                }
            }
            other => inputs.push(other.to_string()),
        }
    }
    if inputs.is_empty() {
        eprintln!("no input file: kime typing <file> [--dict <库>] [--out <报告.md>]");
        return ExitCode::from(2);
    }
    let path = dict_path.unwrap_or_else(default_dict_path);
    let mut failures = 0usize;
    for file in &inputs {
        let text = match std::fs::read_to_string(file) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("failed to read {file}: {e}");
                failures += 1;
                continue;
            }
        };
        // 每个文件重开一次词库：Dict 不是 Clone（持有 SQLite 连接），
        // 而 typing::run 会消费它来建引擎。少量文件 = 少量开库，可接受。
        let dict = match Dict::open(&path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("failed to open dict {}: {e}", path.display());
                return ExitCode::from(1);
            }
        };
        // 语料里的键是**拼音字母**，因此默认按全拼回放；用户配置里的双拼
        // 不继承——否则同一份语料在双拼用户机器上会被解码成别的音节
        // （实测 "jin" 被解成 ji+n，候选全是 技能/继续 之类）。
        let mut config = Config::load();
        config.shuangpin = mode.expect("mode 有默认值");
        let report = kime_core::typing::run(dict, config, &text);
        if json {
            let rows: Vec<String> = report
                .cases
                .iter()
                .map(|c| {
                    format!(
                        "{{\"text\":{},\"lang\":{:?},\"keys\":{},\"rank\":{},\"cands\":{}}}",
                        json_str(&c.text),
                        c.lang,
                        json_str(&c.keys),
                        c.rank
                            .map(|r| r.to_string())
                            .unwrap_or_else(|| "null".into()),
                        c.cands
                    )
                })
                .collect();
            println!(
                "{{\"file\":{},\"summary\":{},\"cases\":[{}]}}",
                json_str(file),
                json_str(&report.summary()),
                rows.join(",")
            );
        } else {
            let md = kime_core::typing::render_markdown(file, &report, limit);
            print!("{md}");
            if let Some(p) = &out {
                match std::fs::write(p, &md) {
                    Ok(()) => println!("报告已写入 {}", p.display()),
                    Err(e) => {
                        eprintln!("failed to write {}: {e}", p.display());
                        failures += 1;
                    }
                }
            }
        }
    }
    if failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
