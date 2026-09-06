//! kime CLI 入口：吞 stdin 拼音串、出候选列表。
//!
//! 用法（M9 起）：
//!   kime build-dict --in <sqlite> --out <bin>
//!   kime config <list|get KEY|set KEY VALUE>
//!   kime --dict <path> [--import <yaml>] [--shuangpin <scheme>]
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
                other => return Err(format!("未知字段: {}", other)),
            }
            if let Some(parent) = config_path.parent() {
                std::fs::create_dir_all(parent);
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
        _ => {}
    }

    let mut dict_path: Option<PathBuf> = None;
    let mut import_path: Option<PathBuf> = None;
    let mut shuangpin: Option<Scheme> = None;
    // 重新迭代（first 已消费）
    let mut args_iter = std::env::args().skip(1).peekable();
    let _action = args_iter.next();

    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "--dict" => {
                dict_path = args_iter.next().map(PathBuf::from);
            }
            "--import" => {
                import_path = args_iter.next().map(PathBuf::from);
            }
            "--shuangpin" => {
                let s = args_iter.next().unwrap_or_default();
                shuangpin = Some(match s.as_str() {
                    "xiaohe" => Scheme::Xiaohe,
                    "ziranma" => Scheme::Ziranma,
                    other => {
                        eprintln!("unknown shuangpin scheme: {other}");
                        return ExitCode::from(2);
                    }
                });
            }
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
                    "用法：\n  kime build-dict --in <sqlite> --out <bin>\n  kime config <list|get KEY|set KEY VALUE>\n  kime --dict <path> [--import <yaml>] [--shuangpin <scheme>]\n  kime repl\n\n  配置字段: shuangpin(xiaohe|ziranma|none), page_size, candidate_limit"
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
    let dict = match dict_path {
        Some(p) => match Dict::open(&p) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("failed to open dict {}: {e}", p.display());
                return ExitCode::from(1);
            }
        },
        None => {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            match Dict::open(PathBuf::from(home).join(".local/share/kime/dict.sqlite3")) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("failed to open default dict: {e}\ntry: kime build-dict --in ... --out ~/.local/share/kime/dict.bin");
                    return ExitCode::from(1);
                }
            }
        }
    };
    let mut dict = dict;
    if let Some(yaml) = import_path {
        if let Err(e) = dict.import(&yaml) {
            eprintln!("import failed: {e}");
            return ExitCode::from(1);
        }
    }
    // CLI --shuangpin 覆盖配置文件里的方案
    let mut config = Config::load();
    if shuangpin.is_some() {
        config.shuangpin = shuangpin;
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
