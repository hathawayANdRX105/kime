//! kime 命令行入口 — M1 的可运行验收。
//!
//! 复用契约：本二进制只与 `kime_core::Engine` 对话；segment / lookup / Dict
//! 全部躲在 Engine 后面。键进 → Outcome 出，跟未来的 `platform-wayland` 壳同构。
//!
//! 用法：
//!   kime-cli [--dict PATH] [--import PATH] [--shuangpin xiaohe|ziranma]
//!
//!   --dict PATH       覆盖 Config 默认词库路径（默认 ~/.local/share/kime/dict.sqlite3）
//!   --import PATH     启动前一次性导入 rime-ice `.dict.yaml` 到词库
//!   --shuangpin NAME  启用双拼方案（xiaohe | ziranma）
//!
//! REPL：stdin 每行一个拼音串；逐字符喂 ASCII 字母 → 行尾喂一次 Space 键
//! （`Key{ch:None, code:57}`）取 Commit。每行打印：
//!   `<preedit>: 1.候选 2.候选 ...`（≤10 个候选），随后 `=> <committed>`（若有）；
//!   无候选的非空行打印 `<line>: (无候选)`；空行或 EOF 退出。

use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::{Engine, Key, Outcome};
use kime_shuangpin::Scheme;

const KEY_SPACE: u32 = 57;
const KEY_ESC: u32 = 1;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut dict_path: Option<String> = None;
    let mut import_path: Option<String> = None;
    let mut shuangpin_scheme: Option<Scheme> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dict" => {
                dict_path = args.next();
            }
            "--import" => {
                import_path = args.next();
            }
            "--shuangpin" => {
                let scheme_str = match args.next() {
                    Some(s) => s,
                    None => {
                        eprintln!("--shuangpin requires a scheme name (xiaohe|ziranma)");
                        return ExitCode::from(2);
                    }
                };
                shuangpin_scheme = match scheme_str.as_str() {
                    "xiaohe" => Some(Scheme::Xiaohe),
                    "ziranma" => Some(Scheme::Ziranma),
                    other => {
                        eprintln!("unknown shuangpin scheme: {other} (expected xiaohe|ziranma)");
                        return ExitCode::from(2);
                    }
                };
            }
            other => {
                eprintln!("unknown arg: {other}");
                return ExitCode::from(2);
            }
        }
    }

    let mut config = Config::default();
    if let Some(p) = dict_path {
        config.dict_path = p;
    }
    config.shuangpin = shuangpin_scheme;

    let mut dict = match Dict::open(&config.dict_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("failed to open dict at {}: {e}", config.dict_path);
            return ExitCode::from(1);
        }
    };

    if let Some(p) = import_path {
        match dict.import(&p) {
            Ok(n) => eprintln!("imported {n} entries from {p}"),
            Err(e) => {
                eprintln!("failed to import {p}: {e}");
                return ExitCode::from(1);
            }
        }
    }

    let mut engine = Engine::new(dict, config);
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    let mut lines = stdin.lock().lines();

    while let Some(line) = lines.next() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("stdin error: {e}");
                return ExitCode::from(1);
            }
        };

        if line.is_empty() {
            // Empty line: reset engine state, print a blank separator, continue.
            // Send Esc to drop any in-flight composition.
            if !engine.preedit().is_empty() {
                let _ = engine.key(Key {
                    ch: None,
                    code: KEY_ESC,
                    shift: false,
                    ctrl: false,
                    alt: false,
                });
            }
            let _ = writeln!(stdout);
            continue;
        }

        // Feed each ASCII char as a printable key.
        for c in line.chars() {
            let _ = engine.key(Key {
                ch: Some(c),
                code: 0,
                shift: c.is_ascii_uppercase(),
                ctrl: false,
                alt: false,
            });
        }

        // Snapshot candidates + preedit, then commit via Space.
        let preedit: String = engine.preedit().to_string();
        let cands: Vec<String> = engine
            .candidates()
            .iter()
            .take(10)
            .map(|c| c.text.clone())
            .collect();

        let outcome = engine.key(Key {
            ch: None,
            code: KEY_SPACE,
            shift: false,
            ctrl: false,
            alt: false,
        });

        if cands.is_empty() {
            let _ = writeln!(stdout, "{line}: (无候选)");
        } else {
            let listed: Vec<String> = cands
                .iter()
                .enumerate()
                .map(|(i, t)| format!("{}.{t}", i + 1))
                .collect();
            let _ = writeln!(stdout, "{preedit}: {}", listed.join(" "));
        }

        if let Outcome::Commit(text) = outcome {
            let _ = writeln!(stdout, "=> {text}");
        }
    }

    ExitCode::SUCCESS
}
