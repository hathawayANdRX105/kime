//! 后台词库写线程 — 选词键路径零 SQLite 写（#101）。
//!
//! # 为什么需要这个线程
//!
//! 选词发生在**持键盘 grab 的单线程事件循环**里：commit_log INSERT、phrase
//! UPSERT、kime_kv 写都要拿 WAL 写锁。离线挖掘（`lm.rs` 的 BEGIN IMMEDIATE
//! 长事务）或双实例并发时，主线程连接的 `busy_timeout = 5000` 会让写等在锁上——
//! 表现为**整个应用的键盘最长冻结 5 秒**。把写挪到本线程后，键路径只剩
//! SELECT；内存提频（「使用即提升当场生效」）仍在同步路径，手感不变。
//!
//! # 线程模型
//!
//! - 队列：`sync_channel(256)` + 键路径 `try_send`——队满丢该 job（静默）。
//!   丢掉的只是日志/学习落盘，绝不允许卡键流。
//! - 连接：线程自持独立 `rusqlite::Connection`（`busy_timeout = 5000`、
//!   `synchronous = NORMAL`；journal_mode 随 DB header，不在此设置）。
//! - 懒 spawn：首个 job 才建线程——纯读使用 Dict 的工具/测试零线程成本。
//! - job 只携带原始字符串：vocab id 在**执行时**经 [`refresh_vocab_cache`]
//!   世代失效后才解析（挖掘重建 vocab 后不许拿陈旧 id 记 commit_log）。
//! - 关停（`Drop` 实现）：置停止标志 → 关 sender → join。
//!   worker 用 `while let Ok(job) = rx.recv()` 收完队列尾部（通道断开后
//!   std 先交付已入队消息），观察到停止标志即把 busy_timeout 切 0 快速排空：
//!   能落尽落、竞争不过就丢，退出绝不挂 256×5s。
//! - 持久化 SQL 与同步版共用 `dict` 模块的 `persist_learn` /
//!   `persist_commit_log`——全仓只有这一份，不许复制第二份。

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::Connection;

use crate::dict::{persist_commit_log, persist_learn, refresh_vocab_cache, today_days, warn_once};

/// 有界队列容量（承重决策：绝不引入 unbounded channel）。256 ≈ 高频选词下
/// 数秒的写入量；满即丢，下一次选词的写自然把计数收敛到最新。
const QUEUE_CAPACITY: usize = 256;

/// worker 线程名（ps/top 里可辨认这条线程是谁）。
const THREAD_NAME: &str = "kime-learn-writer";

/// 键路径入队的写作业。只携带原始字符串——vocab id 与 `today` 都在执行时
/// 现算，排队期间被挖掘/跨天也不会把陈旧 id 或旧日期写进库。
pub(crate) enum LearnJob {
    /// 一次上屏的 commit_log 行（离线 LM 第 0 层原料）。
    CommitLog {
        /// 上一次提交词 `(text, reading)`；None = 本次无上文
        ctx: Option<(String, String)>,
        /// 读音（音节 `'` 连接）
        reading: String,
        text: String,
        /// 落屏句的上屏前文尾（引擎 context，见 engine.rs）
        tail: Option<String>,
    },
    /// 一次用户词提频的持久化（phrase UPSERT + kime_kv 计数）。
    Learn {
        /// 拼音（音节 `'` 连接）
        pinyin: String,
        /// 声母缩写（"ni'hao" → "nh"）
        abbrev: String,
        text: String,
        /// 使用次数与最近使用日：**绝对值**（`learn_mem` 已算好）——队列
        /// 丢一个 job 后下一个 job 仍收敛到最新计数，不做增量。
        n: u64,
        day: u64,
    },
}

/// worker 执行态：vocab id 缓存、世代号、一次性告警哨兵。
struct WriterState {
    vocab_ids: HashMap<(String, String), i64>,
    lm_generation: i64,
    warned: bool,
}

/// Dict 持有的后台写线程句柄（懒 spawn，见模块头线程模型）。
#[derive(Default)]
pub(crate) struct LearnWriter {
    /// 停止标志：Drop 置位后 worker 把剩余 job 按零超时排空。
    stopping: Arc<AtomicBool>,
    tx: Option<SyncSender<LearnJob>>,
    handle: Option<JoinHandle<()>>,
    /// 启动失败/通道断开的一次性告警哨兵（防每键刷屏）。
    warned: bool,
}

impl LearnWriter {
    /// 入队一个作业：键路径唯一可能碰到的「写动作」，`try_send` 绝不阻塞——
    /// 队满静默丢弃；线程起不来/已退出则一次性告警后丢弃，绝不 panic。
    pub(crate) fn submit(&mut self, db_path: &Path, job: LearnJob) {
        let outcome = match self.ensure_running(db_path) {
            Some(tx) => tx.try_send(job),
            None => return,
        };
        if let Err(TrySendError::Disconnected(_)) = outcome {
            warn_once(
                &mut self.warned,
                "[kime] 后台写线程不可用，本条持久化已丢弃（后续不再重复提示）",
            );
        }
    }

    /// 懒 spawn：首个 job 才开线程（纯读使用 Dict 的工具/测试零线程成本）。
    /// spawn 失败保持未启动状态，下一个 job 自动重试。
    fn ensure_running(&mut self, db_path: &Path) -> Option<&SyncSender<LearnJob>> {
        if self.tx.is_some() {
            return self.tx.as_ref();
        }
        let (tx, rx) = sync_channel(QUEUE_CAPACITY);
        let stopping = Arc::clone(&self.stopping);
        let path = db_path.to_path_buf();
        let spawned = std::thread::Builder::new()
            .name(THREAD_NAME.to_string())
            .spawn(move || run_worker(&path, &stopping, rx));
        match spawned {
            Ok(handle) => {
                self.handle = Some(handle);
                self.tx = Some(tx);
                self.tx.as_ref()
            }
            Err(e) => {
                warn_once(
                    &mut self.warned,
                    &format!(
                        "[kime] 后台写线程启动失败，本条持久化已丢弃（后续不再重复提示）: {e}"
                    ),
                );
                None
            }
        }
    }
}

impl Drop for LearnWriter {
    fn drop(&mut self) {
        // 顺序即正确性：先置停止标志（worker 收尾时切零超时），再关 sender
        // （recv 吐完队列尾部后返回断开），最后 join——本函数返回时该落的已落盘。
        self.stopping.store(true, Ordering::Release);
        self.tx = None;
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// worker 主循环：自持连接逐个执行 job，通道断开即排空退出（见模块头）。
fn run_worker(db_path: &Path, stopping: &AtomicBool, rx: Receiver<LearnJob>) {
    let conn = match open_worker_conn(db_path) {
        Ok(conn) => conn,
        Err(e) => {
            // 一次性事件（线程随即退出、后续 job 走 Disconnected 告警）；
            // stderr 写失败直接吞，不许在后台线程 panic。
            let _ = writeln!(
                std::io::stderr(),
                "[kime] 后台写线程打开数据库失败，写入暂停: {e}"
            );
            return;
        }
    };
    let mut state = WriterState {
        vocab_ids: HashMap::new(),
        lm_generation: 0,
        warned: false,
    };
    let mut draining = false;
    while let Ok(job) = rx.recv() {
        if !draining && stopping.load(Ordering::Acquire) {
            // 关停排空阶段：队列里只剩尾巴，拿不到写锁就丢，绝不挂满 5s。
            let _ = conn.busy_timeout(Duration::ZERO);
            draining = true;
        }
        // 每个 job 前查一次世代号（单行 SELECT）：挖掘重建 vocab 后缓存全部
        // 作废，与 set_lm_context 共用同一份失效实现。
        refresh_vocab_cache(&conn, &mut state.lm_generation, &mut state.vocab_ids);
        if let Err(e) = run_job(&conn, &mut state, job) {
            warn_once(
                &mut state.warned,
                &format!("[kime] 后台写入失败（后续不再重复提示）: {e}"),
            );
        }
    }
}

/// 打开 worker 自持连接。PRAGMA 不持久（busy_timeout 每连接都要重设），
/// journal_mode 是持久属性、随 DB header，故不在此设置。
fn open_worker_conn(db_path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(db_path)?;
    conn.execute_batch(
        "PRAGMA busy_timeout = 5000;
         PRAGMA synchronous = NORMAL;",
    )?;
    Ok(conn)
}

/// 执行单个 job：SQL 全部走 dict 模块的共享实现（与同步 API 同一份）。
fn run_job(conn: &Connection, state: &mut WriterState, job: LearnJob) -> rusqlite::Result<()> {
    match job {
        LearnJob::CommitLog {
            ctx,
            reading,
            text,
            tail,
        } => persist_commit_log(
            conn,
            &mut state.vocab_ids,
            ctx.as_ref().map(|(t, r)| (t.as_str(), r.as_str())),
            &reading,
            &text,
            tail.as_deref(),
            today_days(),
        ),
        LearnJob::Learn {
            pinyin,
            abbrev,
            text,
            n,
            day,
        } => persist_learn(conn, &pinyin, &abbrev, &text, n, day),
    }
}
