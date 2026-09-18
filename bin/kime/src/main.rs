//! kime CLI 入口：吞 stdin 拼音串、出候选列表。
//!
//! 用法（M9 起）：
//!   kime build-dict --in <sqlite> --out <bin>
//!   kime config <list|get KEY|set KEY VALUE>
//!   kime english [--dict <path>] <字母串>...
//!   kime --dict <path> [--import <yaml>]... [--import-english <yaml>]... [--shuangpin <scheme>]
//!   kime repl
//!
//! 配置字段: shuangpin(xiaohe|ziranma|none), page_size, candidate_limit

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

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

fn handle_config_cmd(args: &[String]) -> Result<String, String> {
    let config_path = if let Ok(p) = std::env::var("KIME_CONFIG_PATH") {
        std::path::PathBuf::from(p)
    } else {
        match std::env::var("HOME") {
            Ok(home) => std::path::PathBuf::from(home)
                .join(".config")
                .join("kime")
                .join("config.toml"),
            Err(_) => return Err("HOME 未设置".to_string()),
        }
    };
    let mut config = Config::load_from_path(&config_path);
    let sub = args.first().map(String::as_str).unwrap_or("list");
    match sub {
        "list" => {
            let toml_str =
                toml::to_string_pretty(&config).map_err(|e| format!("序列化失败: {}", e))?;
            Ok(format!("{}\n\n{}", config_path.display(), toml_str))
        }
        "get" => {
            let key = args
                .get(1)
                .ok_or_else(|| "用法: kime config get <KEY>".to_string())?;
            match key.as_str() {
                "shuangpin" => Ok(format!("{:?}", config.shuangpin)),
                "page_size" => Ok(config.page_size.to_string()),
                "candidate_limit" => Ok(config.candidate_limit.to_string()),
                "dict_path" => Ok(config.dict_path.clone()),
                "fuzzy" => Ok(format!("{:?}", config.fuzzy)),
                "punct_mode" => Ok(format!("{:?}", config.punct_mode)),
                other => Err(format!("未知字段: {}", other)),
            }
        }
        "set" => {
            let key = args
                .get(1)
                .ok_or_else(|| "用法: kime config set <KEY> <VALUE>".to_string())?;
            let val = args.get(2).ok_or_else(|| "缺少 VALUE".to_string())?;
            match key.as_str() {
                "shuangpin" => {
                    config.shuangpin = match val.as_str() {
                        "xiaohe" => Some(kime_shuangpin::Scheme::Xiaohe),
                        "ziranma" => Some(kime_shuangpin::Scheme::Ziranma),
                        "none" => None,
                        other => return Err(format!("无效值: {} (xiaohe|ziranma|none)", other)),
                    };
                }
                "page_size" => {
                    config.page_size = val
                        .parse()
                        .map_err(|_| "page_size 必须是正整数".to_string())?;
                }
                "candidate_limit" => {
                    config.candidate_limit = val
                        .parse()
                        .map_err(|_| "candidate_limit 必须是正整数".to_string())?;
                }
                "punct_mode" => {
                    config.punct_mode = match val.as_str() {
                        "chinese" => kime_core::config::PunctMode::Chinese,
                        "english" => kime_core::config::PunctMode::English,
                        other => return Err(format!("无效值: {} (chinese|english)", other)),
                    };
                }
                other => return Err(format!("未知字段: {}", other)),
            }
            if let Some(parent) = config_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let toml_str =
                toml::to_string_pretty(&config).map_err(|e| format!("序列化失败: {}", e))?;
            std::fs::write(&config_path, toml_str).map_err(|e| format!("写入失败: {}", e))?;
            Ok(format!("已更新 {} = {}", key, val))
        }
        other => Err(format!(
            "未知子命令: {} (list|get KEY|set KEY VALUE)",
            other
        )),
    }
}

fn main() -> ExitCode {
    let mut args_iter = std::env::args().skip(1).peekable();
    let first = args_iter.next();

    // 子命令分派
    match first.as_deref() {
        Some("config") => {
            let sub_args: Vec<String> = args_iter.collect();
            return match handle_config_cmd(&sub_args) {
                Ok(s) => {
                    println!("{}", s);
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("config error: {}", e);
                    ExitCode::from(2)
                }
            };
        }
        Some("status") => {
            let config = Config::load();
            let mode = if config.punct_mode == kime_core::config::PunctMode::Chinese {
                "chinese"
            } else {
                "english"
            };
            let scheme = format!("{:?}", config.shuangpin);
            println!("mode: {}, scheme: {}", mode, scheme);
            return ExitCode::SUCCESS;
        }
        Some("english") => {
            let sub_args: Vec<String> = args_iter.collect();
            return handle_english_cmd(&sub_args);
        }
        Some("mine-lm") => {
            let mut dict_path: Option<PathBuf> = None;
            let mut sub_args = args_iter.peekable();
            while let Some(arg) = sub_args.next() {
                if arg == "--dict" {
                    dict_path = sub_args.next().map(PathBuf::from);
                }
            }
            let path = dict_path.unwrap_or_else(default_dict_path);
            let start = std::time::Instant::now();
            return match Dict::open(&path) {
                Ok(d) => match kime_core::lm::mine(d.conn()) {
                    Ok(st) => {
                        eprintln!(
                            "mined: log {} rows, admitted {} pairs, evicted {}, purged {}, gen {} in {:?}",
                            st.log_rows, st.admitted, st.evicted, st.purged_rows, st.generation,
                            start.elapsed()
                        );
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
    let mut args_iter = std::env::args().skip(1).peekable();

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
                let s = args_iter.next().unwrap_or_default();
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
            "--help" => {
                println!(
                    "用法：\n  kime build-dict --in <sqlite> --out <bin>\n  kime config <list|get KEY|set KEY VALUE>\n  kime english [--dict <path>] <字母串>...（直查英文词表）\n  kime --dict <path> [--import <yaml>]... [--import-english <yaml>]... [--shuangpin <scheme>]（两个 --import 都可重复）\n  kime repl\n\n  配置字段: shuangpin(xiaohe|ziranma|none), page_size, candidate_limit"
                );
                return ExitCode::SUCCESS;
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
