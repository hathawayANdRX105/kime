//! 剪贴板监视（M16）：后台线程跑 `wl-paste --watch`，文本经 stdout 流回主线程。
//!
//! ponytail: 直接用 wl-clipboard 的 `wl-paste --watch`（本机已装，deskctl 同
//! 依赖），而不是手写 zwlr_data_control 协议——协议要 offer/receive/memfd/
//! roundtrip 一整套，watch 语义（选区变化 → 执行命令、新选区走 stdin）一行
//! 就有。协议直写等剪贴板需要 O(1) 延迟或图片支持时再上。
//!
//! 事件流：每个选区变化 → `sh -c 'cat; printf "\0"'` → 新选区内容 + NUL。
//! Rust 侧按 NUL 切分；GUI 文本不含 NUL（UTF-8 禁止），可无损切分。

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;

/// 给子进程挂 `PR_SET_PDEATHSIG`：父进程一死，内核直接 `SIGKILL` 子进程。
/// 不依赖 `Drop`、不依赖正常退出——崩溃 / `SIGKILL` / `abort` 全兜住。
///
/// 事故背景（2026-09-18）：kime-ime 崩溃时没人认领 `wl-paste --watch`，被 init
/// 收养后永久泄漏；kime-switch 的 2s 崩溃拉起循环把它放大成一次崩溃漏一个，
/// 8 小时积了 272 个孤儿。
///
/// `pub` 是给回归测试复用同一段 `pre_exec`：真实路径的 `wl-paste` 需要 Wayland
/// 合成器，CI 跑不了，测试用 `sleep` 钉住机制本身。
pub fn arm(cmd: &mut Command) {
    let parent = std::process::id() as libc::pid_t;
    // SAFETY: pre_exec 闭包跑在 fork 后、exec 前的子进程里，只允许异步信号安全
    // 调用。这里只有 prctl / getppid / _exit 三个裸 syscall，不碰分配器与 TLS。
    unsafe {
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // fork 与 prctl 之间父进程若已死，死亡事件已过、信号永远不会投递；
            // 自查 ppid 发现已被收养（init/subreaper）就自己退，不留漏网之鱼。
            if libc::getppid() != parent {
                libc::_exit(1);
            }
            Ok(())
        });
    }
}

/// 后台线程：起 wl-paste --watch，逐事件发文本。进程退出/启动失败只打日志
/// （无 wl-clipboard 是可接受降级——剪贴板候选空，其余功能不受影响）。
pub fn spawn(tx: Sender<String>) {
    std::thread::spawn(move || match Watcher::start() {
        Ok(mut w) => w.run(tx),
        Err(e) => eprintln!("[kime-clip] wl-paste 不可用，剪贴板候选停用: {e}"),
    });
}

struct Watcher {
    child: Child,
}

impl Watcher {
    fn start() -> std::io::Result<Self> {
        let mut cmd = Command::new("wl-paste");
        cmd.args([
            "--watch",
            "sh",
            "-c",
            // 选区内容走 stdin；NUL 收尾给 Rust 侧切分事件
            "cat; printf '\\0'",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
        arm(&mut cmd);
        Ok(Self {
            child: cmd.spawn()?,
        })
    }

    fn run(&mut self, tx: Sender<String>) {
        let mut out = self.child.stdout.take().expect("stdout piped");
        let mut buf = [0u8; 4096];
        let mut pending: Vec<u8> = Vec::new();
        loop {
            match out.read(&mut buf) {
                Ok(0) => break, // EOF：wl-paste 退出
                Ok(n) => {
                    pending.extend_from_slice(&buf[..n]);
                    // 切出完整事件（NUL 结尾）
                    while let Some(pos) = pending.iter().position(|&b| b == 0) {
                        let event: Vec<u8> = pending.drain(..=pos).collect();
                        let text = String::from_utf8_lossy(&event[..event.len() - 1]).into_owned();
                        if !text.trim().is_empty() {
                            let _ = tx.send(text);
                        }
                    }
                    // 防炸：无 NUL 的垃圾积压超过 1MB 就放弃重同步
                    if pending.len() > 1024 * 1024 {
                        pending.clear();
                    }
                }
                Err(e) => {
                    eprintln!("[kime-clip] 读 wl-paste 输出失败: {e}");
                    break;
                }
            }
        }
        let _ = self.child.wait();
        eprintln!("[kime-clip] wl-paste --watch 退出");
    }
}
