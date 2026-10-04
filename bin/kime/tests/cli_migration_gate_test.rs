//! 迁移期闸门的行为：`status` / `config` 交给 clap，其余子命令与裸 flag
//! 仍走 legacy 解析且行为不变。
//!
//! 这组测试的价值在**回归方向**上：clap 接管 `status` 之后，`english` /
//! `debug` / `mine-lm` / `build-dict` 与 REPL 的参数解析不能被牵连。
//! REPL 侧另有 `repl_independent_lines_test.rs` 覆盖组合状态不泄漏。

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kime"))
        .args(args)
        .output()
        .expect("execute kime")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// 已迁移的 `status` 仍输出 mode/scheme——既有 `status_cli_test.rs` 之外的
/// 补充：确认走 clap 后输出格式没变。
#[test]
fn status_still_prints_mode_and_scheme() {
    let out = run(&["status"]);
    assert!(out.status.success(), "status failed: {}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("mode:"), "missing mode: {stdout}");
    assert!(stdout.contains("scheme:"), "missing scheme: {stdout}");
}

/// `status` 不接受多余位置参数——迁移前手写 match 直接忽略，
/// 现在由 clap 拒绝。这是迁移带来的**行为收紧**，属预期。
#[test]
fn status_rejects_stray_positional_arg() {
    let out = run(&["status", "oops"]);
    assert!(
        !out.status.success(),
        "stray arg to status must fail, stdout: {}",
        stdout_of(&out)
    );
}

/// `--help` / `--version` 由 clap 生成。迁移前只有手写 `--help` 字符串，
/// 没有 `--version`；两条都应可用了。
#[test]
fn help_and_version_are_available() {
    let help = run(&["--help"]);
    assert!(help.status.success(), "--help failed: {}", stderr_of(&help));
    assert!(
        stdout_of(&help).contains("status"),
        "help should mention migrated subcommands: {}",
        stdout_of(&help)
    );

    let version = run(&["--version"]);
    assert!(
        version.status.success(),
        "--version failed: {}",
        stderr_of(&version)
    );
    assert!(
        stdout_of(&version).contains(env!("CARGO_PKG_VERSION")),
        "--version should print the crate version: {}",
        stdout_of(&version)
    );
}

/// 未迁移子命令 `debug` 仍走 legacy：未知 flag 被静默吞掉是迁移前的既有
/// 行为（见 PR 说明 #2）。此处钉住「迁移没有顺手改它」，后续阶段再单独
/// 收紧——避免两个阶段的行为变更混在一次评审里。
#[test]
fn unmigrated_debug_subcommand_still_runs_legacy() {
    let out = run(&["debug", "--scenario", "commit-backspace"]);
    // 闸门测试的核心是「clap 没抢走这个子命令」——若 clap 接管了 debug，
    // 退出码会是 2 或输出为空。故必须断言成功 + 场景真的跑起来了。
    assert!(
        out.status.success(),
        "debug must still exit 0 under legacy parser: {}",
        stderr_of(&out)
    );
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("Commit(\"你好\")"),
        "scenario should have committed 你好, got: {stdout}"
    );
    // 退格两下都放行应用（面板不重建）——顺带钉住 #104 修的那个行为，
    // 防止 clap 迁移把引擎路径带坏。
    assert_eq!(
        stdout.matches("BACKSPACE  => Ignored").count(),
        2,
        "both backspaces should pass through, got: {stdout}"
    );
    // legacy 独有：clap 的 help 输出不会有这句中文提示。
    assert!(
        stderr_of(&out).contains("fixture"),
        "legacy parser prints the fixture hint: {}",
        stderr_of(&out)
    );
}

/// 未知子命令仍走 legacy 的 `unknown arg` 分支，退出码 2。
#[test]
fn unknown_subcommand_exits_two() {
    let out = run(&["definitely-not-a-command"]);
    assert_eq!(out.status.code(), Some(2), "unknown subcommand must exit 2");
    assert!(
        stderr_of(&out).contains("unknown arg"),
        "legacy parser owns the error text: {}",
        stderr_of(&out)
    );
}
