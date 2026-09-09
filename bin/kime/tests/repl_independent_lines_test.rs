//! REPL 回归测试：每行输入必须独立（上一行的组合状态不得泄漏到下一行）。
//!
//! 复现原 bug：连续两行 "nihao" + "nh"，旧实现第二行变成 "nihaonih" 无候选；
//! 修复后每行先发 ESC 清组合，第二行应命中 abbrev='nh' 的候选。

use std::io::Write;
use std::process::{Command, Stdio};

fn run_repl(input: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kime"))
        .arg("--shuangpin")
        .arg("none")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn kime repl");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin.write_all(input.as_bytes()).expect("write stdin");
    }
    let out = child.wait_with_output().expect("wait");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn repl_lines_are_independent() {
    let out = run_repl("nihao\nnh\n");
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines.len() >= 2, "expected 2 output lines, got: {out}");
    assert!(
        lines[0].contains("你好"),
        "first line should have 你好 candidates: {}",
        lines[0]
    );
    assert!(
        !lines[1].contains("(无候选)"),
        "second line 'nh' must not inherit composition state: {}",
        lines[1]
    );
    assert!(
        lines[1].contains("你好") || lines[1].contains("女孩"),
        "second line 'nh' should hit abbrev candidates: {}",
        lines[1]
    );
}
