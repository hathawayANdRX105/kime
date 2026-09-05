//! kime CLI 入口：吞 stdin 拼音串、出候选列表。
//!
//! 用法（M7 起）：
//!   kime build-dict --in <sqlite> --out <bin>
//!   kime --dict <path> [--import <yaml>]
//!
//! default：组合数据 REPL。
//! build-dict：把 SQLite 词库编译成 FST 二进制词库。

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kime_core::builder::build;
use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::{Engine, Key, Outcome};
use kime_shuangpin::Scheme;

const KEY_SPACE: u32 = 57;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut dict_path: Option<PathBuf> = None;
    let mut import_path: Option<PathBuf> = None;
    let mut shuangpin: Option<Scheme> = None;
    let mut args_iter = args.peekable();
    let action = args_iter.next();

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
                    "用法：\n  kime build-dict --in <sqlite> --out <bin>\n  kime --dict <path> [--import <yaml>] [--shuangpin <scheme>]\n\n  --build-dict 就把 rime-ice 词库编译为 FST 二进制，节省 80% 内存。"
                );
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown arg: {other}");
                return ExitCode::from(2);
            }
        }
    }

    let _ = action;
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
            match Dict::open(PathBuf::from(format!(
                "{}/.local/share/kime/dict.sqlite3",
                home
            ))) {
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
    let mut engine = Engine::new(dict, Config::default());
    if let Some(sp) = shuangpin {
        let _ = sp;
    }

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let input = line.trim().to_string();
        if input.is_empty() {
            continue;
        }
        let mut outcome = None;
        for c in input.bytes() {
            if !(c.is_ascii_lowercase()) {
                continue;
            }
            let key = Key {
                ch: Some(c as char),
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
        if let Outcome::Commit(text) = outcome {
            let _ = writeln!(stdout, "=> {text}");
        }
    }
    ExitCode::SUCCESS
}
