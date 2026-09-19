//! 回归：父进程异常死亡时，`clipboard_watch::arm` 挂的子进程必须被内核回收。
//!
//! 事故背景（2026-09-18）：kime-ime 崩溃时 `wl-paste --watch` 无人认领，被 init
//! 收养后永久泄漏；kime-switch 的 2s 崩溃拉起循环放大成一次崩溃漏一个，8 小时
//! 积了 272 个孤儿。修复靠 `PR_SET_PDEATHSIG`——它不依赖 `Drop`、不依赖正常退出。
//!
//! 真机路径的 `wl-paste` 需要 Wayland 合成器，CI 没有；机制与具体命令无关，
//! 所以这里用 `sleep` 钉住机制本身：`arm` 过的东西必须随父进程一起死。
//!
//! 两段式（测试进程不能杀死自己再断言）：本测试在 helper 模式下 spawn 一个 armed
//! `sleep` 并写下它的 pid，然后让进程**正常结束**——父进程终止，内核就该立刻
//! `SIGKILL` 掉 `sleep`。父侧再确认它确实没了。未加 `arm` 时 `sleep` 会活满 30s，
//! 测试即失败。

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MODE: &str = "KIME_ORPHAN_HELPER";
const PID_FILE: &str = "KIME_ORPHAN_PID_FILE";
const TEST_NAME: &str = "armed_child_dies_when_parent_exits";

#[test]
fn armed_child_dies_when_parent_exits() {
    match std::env::var_os(MODE) {
        Some(_) => helper(),
        None => parent(),
    }
}

/// helper 侧：起一个 armed `sleep`，交回 pid，然后让进程退出。
fn helper() {
    let mut cmd = Command::new("sleep");
    cmd.arg("30");
    platform_wayland::clipboard_watch::arm(&mut cmd);
    let child = cmd.spawn().expect("spawn sleep");
    let path = std::env::var(PID_FILE).expect("helper 缺 pid 文件路径");
    std::fs::write(path, child.id().to_string()).expect("写 pid 文件");
    // 不 wait、不 kill：父进程随即终止，回收是内核的活
}

/// 父侧：跑 helper，等它结束，要求 armed 子进程同时消失。
fn parent() {
    let pid_file = std::env::temp_dir().join(format!("kime-orphan-{}.pid", std::process::id()));
    let _ = std::fs::remove_file(&pid_file);

    let helper = Command::new(std::env::current_exe().expect("current_exe"))
        .args(["--exact", TEST_NAME])
        .env(MODE, "1")
        .env(PID_FILE, &pid_file)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("起 helper");

    let pid = read_pid(&pid_file);
    let status = helper.wait_with_output().expect("等 helper");
    assert!(status.status.success(), "helper 未正常退出");
    let _ = std::fs::remove_file(&pid_file);

    // helper 已死 → 内核必须已经给 armed 子进程发过 SIGKILL
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    // 留证据不如清干净
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    panic!("armed 子进程 pid={pid} 在父进程退出后仍存活 = 孤儿回归（PDEATHSIG 失效）");
}

fn read_pid(path: &std::path::Path) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(s) = std::fs::read_to_string(path) {
            if let Ok(pid) = s.trim().parse() {
                return pid;
            }
        }
        assert!(Instant::now() < deadline, "helper 10s 内没写出 sleep pid");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 还活着且不是僵尸（僵尸即已收到信号、等回收，算死）。
fn alive(pid: i32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .split_whitespace()
            .nth(2)
            .is_some_and(|state| state != "Z"),
        Err(_) => false,
    }
}
