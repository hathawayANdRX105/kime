//! kime 性能基准（零依赖，harness = false，直接 `cargo bench --bench kime_bench`）。
//!
//! 计量口径：
//! 1. 每个度量先跑 1 轮预热（不计时），排除 mmap 缺页与分支预测器/缓存冷态对首轮样本的污染。
//! 2. 每个度量 5 轮（REPS）独立计时，报 best/median。
//! 3. 工作负载返回值逐轮累加成 u64 校验和，结果被消费，编译器无法 DCE 掉工作负载；
//!    数值漂移即行为回归信号。
//! 4. FST 词库缺失/打开失败只跳过该段，不中断后续度量。
//!
//! 词库路径取 KIME_BENCH_DB，FST 取 KIME_BENCH_BIN。

use std::path::Path;
use std::time::Instant;

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::engine::{Engine, Key, Outcome, KEY_SPACE};
use kime_core::store::FstStore;

/// 每个度量的独立计时轮数（报 best/median）。
const REPS: usize = 5;

/// 升序排序后取中位（REPS 为奇数，无插值）。
fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}

/// 跑 1 轮工作负载（内部循环 `n` 次），返回 (本轮耗时折算到每次的毫秒, 本轮校验和)。
fn run_round(n: usize, mut round: impl FnMut() -> u64) -> (f64, u64) {
    let t0 = Instant::now();
    let checksum = round();
    (t0.elapsed().as_secs_f64() * 1000.0 / n as f64, checksum)
}

/// 预热 1 轮（不计时）+ REPS 轮计时，打印 best/median，返回计时轮的校验和累加。
fn measure(label: &str, unit: &str, n: usize, mut round: impl FnMut() -> u64) -> u64 {
    // 预热：完整跑一轮，结果丢弃。mmap 缺页 / 缓存与分支预测器冷态只影响首轮。
    let _ = round();

    let mut samples = [0.0f64; REPS];
    let mut checksum = 0u64;
    for s in samples.iter_mut() {
        let (ms, cs) = run_round(n, &mut round);
        *s = ms;
        checksum = checksum.wrapping_add(cs);
    }
    let best = samples.iter().copied().fold(f64::INFINITY, f64::min);
    let med = median(&mut samples);
    println!("{label}: {best:.4}/{med:.4}ms/{unit} (best/median, {REPS}轮×{n}{unit})");
    checksum
}

fn main() {
    let db_path = std::env::var("KIME_BENCH_DB")
        .unwrap_or_else(|_| "/home/hathaway/.local/share/kime/dict.sqlite3".to_string());
    let bin_path =
        std::env::var("KIME_BENCH_BIN").unwrap_or_else(|_| "/tmp/kime_dict.bin".to_string());
    println!("=== kime 性能基准测试 ===");
    println!("词库: {db_path} / FST: {bin_path}");

    if !Path::new(&db_path).exists() {
        eprintln!("词库文件未找到: {}，请先运行数据准备", db_path);
        return;
    }

    // 全局校验和：各段工作负载返回值之和，防止死代码消除 + 暴露行为漂移。
    let mut checksum: u64 = 0;

    // 1. SQLite 加载时间（冷载本就单次，取中位无意义）
    let t0 = Instant::now();
    let dict = match Dict::open(&db_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("打开 SQLite 失败: {e}");
            return;
        }
    };
    println!(
        "1. SQLite 载入 + 内存双索引: {:.3}s（冷载，不取中位）",
        t0.elapsed().as_secs_f64()
    );

    // 2. 精确前缀查询 (ni'hao)
    let queries = [
        ("ni'hao", vec!["ni".to_string()], "hao"),
        ("shen'me", vec!["shen".to_string()], "me"),
        ("ni'ha", vec!["ni".to_string()], "ha"),
        ("g'x", vec!["g".to_string()], "x"),
    ];

    for (label, syllables, tail) in &queries {
        let n = 200;
        checksum =
            checksum.wrapping_add(measure(&format!("2. 查询 [{label}]"), "次", n, || {
                let mut cs = 0u64;
                for _ in 0..n {
                    cs += dict
                        .lookup_prefix(syllables, tail, 10)
                        .unwrap_or_default()
                        .len() as u64;
                }
                cs
            }));
    }

    // 3. 缩写查询 (nh)
    {
        let n = 500;
        checksum = checksum.wrapping_add(measure("3. 缩写查询 [nh]", "次", n, || {
            let mut cs = 0u64;
            for _ in 0..n {
                cs += dict.lookup_abbrev("nh", 10).unwrap_or_default().len() as u64;
            }
            cs
        }));
    }

    // 4. FST 查询（缺失或打开失败只跳过本段，后续度量照常）
    let store: Option<FstStore> = if Path::new(&bin_path).exists() {
        let t0 = Instant::now();
        match FstStore::open(Path::new(&bin_path)) {
            Ok(s) => {
                println!(
                    "4. FST 载入 (mmap): {:.3}s（冷载，不取中位）",
                    t0.elapsed().as_secs_f64()
                );
                Some(s)
            }
            Err(e) => {
                println!("4. FST 打开失败: {e}，跳过 FST 查询");
                None
            }
        }
    } else {
        println!("4. FST 未就绪（无 {bin_path}），跳过");
        None
    };

    if let Some(store) = &store {
        for (label, syllables, tail) in &queries {
            let n = 500;
            checksum =
                checksum.wrapping_add(measure(&format!("   FST [{label}]"), "次", n, || {
                    let mut cs = 0u64;
                    for _ in 0..n {
                        cs += store.lookup_prefix(syllables, tail, 10).len() as u64;
                    }
                    cs
                }));
        }
    }

    // 5. Viterbi 整句联想（按键热路径，O(n^2) 次 dict.lookup）
    let sentences: [&[&str]; 4] = [
        &["ni", "hao"],
        &["ni", "hao", "shi", "jie"],
        &["wo", "men", "yi", "qi", "qu", "chi", "fan"],
        &[
            "jin", "tian", "tian", "qi", "zhen", "de", "hen", "bu", "cuo", "a", "ni", "yao", "bu",
            "yao", "chu", "qu", "zou", "zou",
        ],
    ];
    for syls in &sentences {
        let reading: Vec<String> = syls.iter().map(|s| s.to_string()).collect();
        let n = 50;
        let label = format!("5. Viterbi 整句 [{} 音节]", reading.len());
        checksum = checksum.wrapping_add(measure(&label, "次", n, || {
            let mut cs = 0u64;
            for _ in 0..n {
                cs += kime_core::lattice::viterbi_sentences(&dict, &reading).len() as u64;
            }
            cs
        }));
    }

    // 6. 引擎按键热路径（真实打字路径：n-i-h-a-o 逐字符 + 空格提交，共 6 键）
    let cfg = Config {
        dict_path: db_path.clone(),
        ..Default::default()
    };
    let mut script: Vec<Key> = "nihao"
        .chars()
        .map(|c| Key {
            ch: Some(c),
            code: 0,
            shift: false,
            ctrl: false,
            alt: false,
        })
        .collect();
    script.push(Key {
        ch: None,
        code: KEY_SPACE,
        shift: false,
        ctrl: false,
        alt: false,
    });
    let keys = script.len();

    // 预热：单独一轮，不计时，且不混入计时轮。
    {
        let mut e = Engine::new(Dict::open(&db_path).expect("预热词库打开失败"), cfg.clone());
        for k in &script {
            let _ = e.key(*k);
        }
    }

    let mut samples = [0.0f64; REPS];
    let mut commits = 0u64;
    let mut dirty_preedit = 0u64;
    for s in samples.iter_mut() {
        // 每轮新开 dict+engine：不跨轮累积 learn 状态，样本之间可比。
        let mut e = Engine::new(Dict::open(&db_path).expect("词库打开失败"), cfg.clone());
        let t0 = Instant::now();
        for k in &script {
            if let Outcome::Commit(text) = e.key(*k) {
                commits += 1;
                checksum = checksum.wrapping_add(text.chars().count() as u64);
            }
        }
        *s = t0.elapsed().as_secs_f64() * 1000.0 / keys as f64;
        if !e.preedit().is_empty() {
            dirty_preedit += 1;
        }
    }
    let best = samples.iter().copied().fold(f64::INFINITY, f64::min);
    let med = median(&mut samples);
    println!(
        "6. 引擎按键热路径 [nihao+空格, {keys}键]: {best:.4}/{med:.4}ms/键 (best/median, {REPS}轮×{keys}键)"
    );
    println!("   sanity: {REPS} 轮 Commit 合计 {commits} 次（预期 {REPS}），preedit 非空 {dirty_preedit} 轮（预期 0）");

    println!("校验和: {checksum:#016x}");
    println!("===========================");
}
