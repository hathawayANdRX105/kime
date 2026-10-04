//! `kime config` 的行为测试：键名校验前移到 clap 后的取舍域回归。
//!
//! 迁移前这些校验都在运行时 match 里（`未知字段` / `无效值`），迁移后
//! 键名由 clap 的 `ValueEnum` 在解析期拒绝，取值域由 `parse_config_value`
//! 按目标键判定。此处钉住可观察行为：哪个键可读、哪个键只读、非法取值
//! 是否被拒且不落盘。

use std::path::Path;
use std::process::{Command, Output};

/// 在临时目录里跑 `kime config ...`，配置文件指向临时路径。
fn run_config(dir: &Path, args: &[&str]) -> Output {
    let config = dir.join("config.toml");
    Command::new(env!("CARGO_BIN_EXE_kime"))
        .arg("config")
        .args(args)
        .env("KIME_CONFIG_PATH", &config)
        .output()
        .expect("execute kime config")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// 旧实现的 `kime config list` 输出首行是配置文件路径，后面跟 TOML 正文。
/// 省略子命令时必须等价于 `list`。
#[test]
fn bare_config_equals_list() {
    let dir = tempfile::tempdir().unwrap();
    let bare = run_config(dir.path(), &[]);
    let list = run_config(dir.path(), &["list"]);
    assert!(
        bare.status.success(),
        "bare config should succeed: {}",
        stderr_of(&bare)
    );
    assert_eq!(
        stdout_of(&bare),
        stdout_of(&list),
        "bare `kime config` must behave exactly as `kime config list`"
    );
    assert!(
        stdout_of(&bare).contains("config.toml"),
        "list output should name the config file: {}",
        stdout_of(&bare)
    );
}

/// `get` 覆盖迁移前支持的 6 个字段，值必须与写入 TOML 的内容一致。
#[test]
fn get_reads_every_supported_key() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "page_size = 7\ncandidate_limit = 13\ndict_path = \"/tmp/d.sqlite3\"\n",
    )
    .unwrap();

    for (key, expected) in [
        ("page_size", "7"),
        ("candidate_limit", "13"),
        ("dict_path", "/tmp/d.sqlite3"),
    ] {
        let out = run_config(dir.path(), &["get", key]);
        assert!(
            out.status.success(),
            "get {key} failed: {}",
            stderr_of(&out)
        );
        assert_eq!(
            stdout_of(&out).trim(),
            expected,
            "get {key} returned wrong value"
        );
    }
}

/// 键名非法在解析期就被拒（退出码 2），且提示里列出合法键——迁移前是运行时
/// 的 `未知字段: xxx`，现在由 clap 生成。
#[test]
fn unknown_key_is_rejected_with_valid_keys_listed() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_config(dir.path(), &["get", "nope"]);
    assert_eq!(out.status.code(), Some(2), "unknown key should exit 2");
    let err = stderr_of(&out);
    assert!(
        err.contains("nope"),
        "error should name the offending key: {err}"
    );
    assert!(
        err.contains("page_size"),
        "error should list valid keys: {err}"
    );
}

/// `set` 后 `get` 必须读到新值，且真的落盘——这是迁移前后都必须成立的不变式。
#[test]
fn set_then_get_round_trips_through_disk() {
    let dir = tempfile::tempdir().unwrap();
    let set = run_config(dir.path(), &["set", "page_size", "9"]);
    assert!(set.status.success(), "set failed: {}", stderr_of(&set));

    let get = run_config(dir.path(), &["get", "page_size"]);
    assert_eq!(stdout_of(&get).trim(), "9");

    // 直接读文件确认落盘，不经进程内存。
    let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(
        raw.contains("page_size = 9"),
        "page_size must persist to disk: {raw}"
    );
}

/// 取值域按**目标键**判定：`punct_mode` 只吃 chinese|english，
/// 即便 `none` 是合法的 shuangpin 取值也不能被误接收。
#[test]
fn value_domain_is_scoped_to_its_key() {
    let dir = tempfile::tempdir().unwrap();
    // none 对 shuangpin 合法
    let ok = run_config(dir.path(), &["set", "shuangpin", "none"]);
    assert!(
        ok.status.success(),
        "shuangpin none is valid: {}",
        stderr_of(&ok)
    );
    assert_eq!(
        stdout_of(&run_config(dir.path(), &["get", "shuangpin"])).trim(),
        "None"
    );

    // 但对 punct_mode 非法——迁移前的多键 union 解析会把它错当成 shuangpin
    let bad = run_config(dir.path(), &["set", "punct_mode", "none"]);
    assert_eq!(
        bad.status.code(),
        Some(2),
        "punct_mode none must be rejected"
    );
    assert!(
        stderr_of(&bad).contains("chinese"),
        "error should list punct_mode's domain: {}",
        stderr_of(&bad)
    );
    // 且不得改写 punct_mode
    assert_eq!(
        stdout_of(&run_config(dir.path(), &["get", "punct_mode"])).trim(),
        "Chinese"
    );
}

/// 非数值键收数值时必须报错并说明期望，而不是静默写入 0。
#[test]
fn numeric_key_rejects_non_numeric_value() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_config(dir.path(), &["set", "page_size", "abc"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "non-numeric page_size must exit 2"
    );
    assert!(
        stderr_of(&out).contains("正整数"),
        "error should state the expected domain: {}",
        stderr_of(&out)
    );
    let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap_or_default();
    assert!(
        !raw.contains("page_size = 0"),
        "rejected set must not persist a coerced zero: {raw}"
    );
}

/// dict_path / fuzzy 可读不可写：迁移前 `set` 落到 `未知字段`，迁移后给出
/// 明确的「只读」原因——比前者更可解释，且同样不改配置。
#[test]
fn read_only_keys_cannot_be_set() {
    let dir = tempfile::tempdir().unwrap();
    for key in ["dict_path", "fuzzy"] {
        let get = run_config(dir.path(), &["get", key]);
        assert!(
            get.status.success(),
            "{key} should be readable: {}",
            stderr_of(&get)
        );

        let set = run_config(dir.path(), &["set", key, "whatever"]);
        assert_eq!(set.status.code(), Some(2), "{key} must not be settable");
        assert!(
            stderr_of(&set).contains("只读"),
            "error should say the field is read-only: {}",
            stderr_of(&set)
        );
    }
}

/// `set` 缺 VALUE 必须报错——迁移前是 `缺少 VALUE`，现在由 clap 的必填位
/// 处理，仍是非零退出。
#[test]
fn set_without_value_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_config(dir.path(), &["set", "page_size"]);
    assert!(
        !out.status.success(),
        "missing VALUE must fail: {}",
        stdout_of(&out)
    );
}

/// 未知 config 子命令被 clap 在解析期拒绝（退出码 2），不再落到运行时的
/// `未知子命令` 分支。
///
/// 注意断言边界：clap 对完全无关的名字只印一行 usage，不列合法子命令
/// （只有 `lst` 这类近似拼错才会给 did-you-mean）。此处钉住的是
/// 「解析期拒绝 + 退出码 2」，不是 clap 的建议文案。
#[test]
fn unknown_config_subcommand_is_rejected_at_parse_time() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_config(dir.path(), &["frobnicate"]);
    assert_eq!(out.status.code(), Some(2), "unknown subcommand must exit 2");
    let err = stderr_of(&out);
    assert!(
        err.contains("unrecognized subcommand"),
        "clap should own the diagnostic: {err}"
    );
    assert!(
        !err.contains("未知子命令"),
        "legacy runtime error must no longer be reachable: {err}"
    );
}
