use std::path::Path;
use std::time::Instant;

fn main() {
    println!("=== kime 性能基准测试 ===");
    let db_path = "/home/hathaway/.local/share/kime/dict.sqlite3";
    let bin_path = "/tmp/kime_dict.bin";

    if !Path::new(db_path).exists() {
        eprintln!("词库文件未找到: {}，请先运行数据准备", db_path);
        return;
    }

    // 1. SQLite 加载时间
    let t0 = Instant::now();
    let dict = match kime_core::dict::Dict::open(db_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("打开 SQLite 失败: {e}");
            return;
        }
    };
    println!(
        "1. SQLite 载入 + 内存双索引: {:.3}s",
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
        let t = Instant::now();
        let n = 200;
        for _ in 0..n {
            let _ = dict.lookup_prefix(syllables, tail, 10);
        }
        println!(
            "2. 查询 [{label}]: {:.4}ms/次",
            t.elapsed().as_secs_f64() * 1000.0 / n as f64
        );
    }

    // 3. 缩写查询 (nh)
    let t = Instant::now();
    let n = 500;
    for _ in 0..n {
        let _ = dict.lookup_abbrev("nh", 10);
    }
    println!(
        "3. 缩写查询 [nh]: {:.4}ms/次",
        t.elapsed().as_secs_f64() * 1000.0 / n as f64
    );

    // 4. FST 查询 (如果存在)
    if Path::new(bin_path).exists() {
        let t = Instant::now();
        let store = match kime_core::store::FstStore::open(Path::new(bin_path)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("打开 FST 失败: {e}");
                return;
            }
        };
        println!("4. FST 载入 (mmap): {:.3}s", t.elapsed().as_secs_f64());

        for (label, syllables, tail) in &queries {
            let t = Instant::now();
            let n = 500;
            for _ in 0..n {
                let _ = store.lookup_prefix(syllables, tail, 10);
            }
            println!(
                "   FST [{label}]: {:.4}ms/次",
                t.elapsed().as_secs_f64() * 1000.0 / n as f64
            );
        }
    } else {
        println!("4. FST 未就绪（无 /tmp/kime_dict.bin），跳过");
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
        let t = Instant::now();
        let n = 50;
        for _ in 0..n {
            let _ = kime_core::lattice::viterbi_sentence(&dict, &reading);
        }
        println!(
            "5. Viterbi 整句 [{} 音节]: {:.4}ms/次",
            reading.len(),
            t.elapsed().as_secs_f64() * 1000.0 / n as f64
        );
    }
    println!("===========================");
}
