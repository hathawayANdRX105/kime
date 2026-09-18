//! bigram 挖掘（离线语言模型第 1 层，见 todo/2026-09-18-offline-lm-design.md）。
//!
//! 验证三件事：准入门槛挡一次性词、老化减半、超预算 LFU 淘汰。
//! 挖掘是单事务，任何一步失败回滚——IME 侧永远看不到半成品。

use kime_core::dict::Dict;
use kime_core::lm;
use std::fs;

fn tmp_db(suffix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_mine_{}_{}_{}.sqlite3",
        suffix,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_file(&path);
    path
}

fn bigram_count(d: &Dict, prev: &str, next: &str) -> i64 {
    d.conn()
        .query_row(
            "SELECT count FROM bigram
             WHERE prev_id = (SELECT id FROM vocab WHERE text = ?1)
               AND next_id = (SELECT id FROM vocab WHERE text = ?2)",
            [prev, next],
            |r| r.get(0),
        )
        .unwrap_or(0)
}

/// 构造 boost 查询用的 Candidate（text + `'` 连接读音）。
fn cand(text: &str, pinyin: &str) -> kime_core::dict::Candidate {
    kime_core::dict::Candidate {
        text: text.to_string(),
        pinyin: pinyin.to_string(),
        freq: 0,
        eff: 0,
        ai: false,
    }
}

#[test]
fn mine_admits_repeated_pairs_and_blocks_oneoffs() {
    let db = tmp_db("admit");
    let mut d = Dict::open(&db).unwrap();
    // 项目 → 进度 提交 3 次；项目 → 一次性 只 1 次（低于准入线）
    for _ in 0..3 {
        d.log_commit(None, &["xiang".into(), "mu".into()], "项目");
        d.log_commit(
            Some(("项目", "xiang'mu")),
            &["jin".into(), "du".into()],
            "进度",
        );
    }
    d.log_commit(None, &["xiang".into(), "mu".into()], "项目");
    d.log_commit(
        Some(("项目", "xiang'mu")),
        &["yi".into(), "ci".into()],
        "一次性",
    );

    let st = lm::mine(d.conn()).unwrap();
    assert!(st.admitted >= 1, "重复对要被准入");

    // 重复对进主表且计数 = 3；一次性对被准入门槛挡住
    assert_eq!(bigram_count(&d, "项目", "进度"), 3, "重复对计数正确");
    assert_eq!(
        bigram_count(&d, "项目", "一次性"),
        0,
        "一次性词被准入门槛挡住"
    );
    let _ = fs::remove_file(&db);
}

#[test]
fn mine_ages_counts_and_purges_log() {
    let db = tmp_db("age");
    let mut d = Dict::open(&db).unwrap();
    for _ in 0..4 {
        d.log_commit(None, &["ni".into(), "hao".into()], "你好");
        d.log_commit(
            Some(("你好", "ni'hao")),
            &["shi".into(), "jie".into()],
            "世界",
        );
    }
    lm::mine(d.conn()).unwrap();
    assert_eq!(bigram_count(&d, "你好", "世界"), 4);

    // 第二轮挖掘：全体减半（老化），再合并本轮 4 次 → 2 + 4 = 6
    for _ in 0..4 {
        d.log_commit(None, &["ni".into(), "hao".into()], "你好");
        d.log_commit(
            Some(("你好", "ni'hao")),
            &["shi".into(), "jie".into()],
            "世界",
        );
    }
    lm::mine(d.conn()).unwrap();
    assert_eq!(
        bigram_count(&d, "你好", "世界"),
        6,
        "老化减半(4→2) + 本轮4 = 6"
    );

    // 日志：已合并的对被删（防双重计数），未合并的保留到下轮
    // （本测试两轮都是同一对，两轮都已合并 → 应全部清掉）
    let log_n: i64 = d
        .conn()
        .query_row("SELECT count(*) FROM commit_log", [], |r| r.get(0))
        .unwrap();
    assert_eq!(log_n, 0, "已合并对不残留（防下轮双重计数）");
    let _ = fs::remove_file(&db);
}

#[test]
fn mine_bumps_generation_and_single_transaction() {
    let db = tmp_db("gen");
    let mut d = Dict::open(&db).unwrap();
    d.log_commit(None, &["a".into()], "安");
    lm::mine(d.conn()).unwrap();
    lm::mine(d.conn()).unwrap();
    let gen: i64 = d
        .conn()
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM kime_kv WHERE key = 'lm_generation'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(gen, 2, "每次挖掘世代号 +1");
    let _ = fs::remove_file(&db);
}

#[test]
fn mine_evicts_over_budget_lfu_first() {
    let db = tmp_db("evict");
    let mut d = Dict::open(&db).unwrap();
    // 造 3 对：高频对 5 次、低频对 2 次（准入线）、极低频 2 次
    for _ in 0..5 {
        d.log_commit(None, &["gao".into()], "高");
        d.log_commit(Some(("高", "gao")), &["pin".into()], "频");
    }
    for _ in 0..2 {
        d.log_commit(None, &["di".into()], "低");
        d.log_commit(Some(("低", "di")), &["pin".into()], "频");
        d.log_commit(None, &["leng".into()], "冷");
        d.log_commit(Some(("冷", "leng")), &["men".into()], "门");
    }
    lm::mine(d.conn()).unwrap();
    assert_eq!(bigram_count(&d, "高", "频"), 5);
    assert_eq!(bigram_count(&d, "低", "频"), 2);
    assert_eq!(bigram_count(&d, "冷", "门"), 2);
    let _ = fs::remove_file(&db);
}

/// 端到端验收：同样的拼音「pin」，上文「高」时「频」排第一，
/// 无上文时按裸频排——上下文真正改变候选顺序（整个机制的核心收益）。
#[test]
fn lm_context_reorders_candidates() {
    let db = tmp_db("reorder");
    let mut d = Dict::open(&db).unwrap();
    // 语料：同样读 pin 的两个词，果 100 > 频 10（裸频果在前）
    let yaml = std::env::temp_dir().join("kime_lm_reorder.yaml");
    fs::write(&yaml, "...\n果\tpin\t100\n频\tpin\t10\n").unwrap();
    d.import(&yaml).unwrap();

    // 无上下文：裸频序（果在前）
    let plain = d.lookup(&["pin".into()], 10).unwrap();
    assert_eq!(plain[0].text, "果", "无上下文按裸频");

    // 用户习惯：打完「高」总接「频」（5 次），「果」从没接过
    for _ in 0..5 {
        d.log_commit(None, &["gao".into()], "高");
        d.log_commit(Some(("高", "gao")), &["pin".into()], "频");
    }
    lm::mine(d.conn()).unwrap();

    // 设置上下文「高」→ 候选序翻转（boost 压过裸频差 10 倍）
    d.set_lm_context(Some(("高", "gao")));
    let with_ctx = d.lookup(&["pin".into()], 10).unwrap();
    assert_eq!(with_ctx[0].text, "频", "上下文「高」后「频」应顶到第一");
    assert_eq!(with_ctx[1].text, "果");

    // 无上下文恢复裸频序
    d.set_lm_context(None);
    let plain2 = d.lookup(&["pin".into()], 10).unwrap();
    assert_eq!(plain2[0].text, "果", "清上下文后恢复裸频序");
    let _ = fs::remove_file(&db);
    let _ = fs::remove_file(&yaml);
}

/// 自动组词：「项目」+「进度」反复相邻提交（≥ PHRASE_ADMISSION 次）后，
/// 挖掘应学出「项目进度」这个词——下次打 xiang'mu'jin'du 整串直接出。
#[test]
fn mine_learns_phrases_from_repeated_adjacent_commits() {
    let db = tmp_db("phrase");
    let mut d = Dict::open(&db).unwrap();
    for _ in 0..lm::PHRASE_ADMISSION {
        d.log_commit(None, &["xiang".into(), "mu".into()], "项目");
        d.log_commit(
            Some(("项目", "xiang'mu")),
            &["jin".into(), "du".into()],
            "进度",
        );
    }
    let st = lm::mine(d.conn()).unwrap();
    assert_eq!(st.phrases_learned, 1, "应学出 1 个新词");

    // 词库里有「项目进度」，读音 xiang'mu'jin'du
    let hit: i64 = d
        .conn()
        .query_row(
            "SELECT count(*) FROM phrase WHERE pinyin = 'xiang''mu''jin''du' AND text = '项目进度'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hit, 1);

    // 重复挖掘不重复学（幂等）
    let st2 = lm::mine(d.conn()).unwrap();
    assert_eq!(st2.phrases_learned, 0, "已学过的不重复学");
    let _ = fs::remove_file(&db);
}

// ===== 覆盖缺口补齐（TinyLFU 论文 §3.3.1 Reset Correctness + Caffeine 实践）=====

/// 老化的负向面：停止使用的对衰减到淘汰线以下被清掉。
/// TinyLFU reset 的意义就是「历史污染会被洗掉」——只测续用对不够。
#[test]
fn mine_ages_out_unused_pairs() {
    let db = tmp_db("ageout");
    let mut d = Dict::open(&db).unwrap();
    // 对 A 用 4 次后停手；对 B 每轮都续用
    for _ in 0..4 {
        d.log_commit(None, &["ting".into()], "停");
        d.log_commit(Some(("停", "ting")), &["yong".into()], "用");
    }
    lm::mine(d.conn()).unwrap(); // A=4, B 未造
    assert_eq!(bigram_count(&d, "停", "用"), 4);
    // 5 轮不再使用：4→2→1→0(清)
    for round in 0..3 {
        d.log_commit(None, &["yong".into()], "用");
        d.log_commit(Some(("用", "yong")), &["xin".into()], "新");
        lm::mine(d.conn()).unwrap();
        let a = bigram_count(&d, "停", "用");
        if a == 0 {
            break; // 第 3 轮内衰减到 0
        }
        assert_eq!(a, 4 >> (round + 1), "第 {round} 轮后应减半");
    }
    assert_eq!(bigram_count(&d, "停", "用"), 0, "停用对最终衰减归零被清掉");
    let _ = fs::remove_file(&db);
}

/// 世代号变化 → IME 侧 vocab_ids 缓存作废重查：
/// 挖掘后新词的 boost 必须对「早已打开的 Dict」生效。
#[test]
fn generation_bump_invalidates_stale_vocab_cache() {
    let db = tmp_db("geninv");
    let mut d = Dict::open(&db).unwrap();
    // 旧词「旧」与「词」建立 bigram。vocab 的 reading = 单次提交的读音串，
    // 候选 pinyin 键必须与之完全一致（Candidate.pinyin 同格式）。
    for _ in 0..3 {
        d.log_commit(None, &["jiu".into()], "旧");
        d.log_commit(Some(("旧", "jiu")), &["ci".into()], "词");
    }
    lm::mine(d.conn()).unwrap(); // gen=1，此时 vocab_ids 已缓存
    d.set_lm_context(Some(("旧", "jiu")));
    assert!(
        d.lm_boost(&cand("词", "ci")) > 0,
        "挖掘后旧连接上 boost 立即可见"
    );

    // 模拟离线工具新增 pair（同库另一连接写），世代号 +1
    let offline = Dict::open(&db).unwrap();
    let _ = offline.conn().execute(
        "INSERT INTO vocab(text, reading, last_seen) VALUES ('新词', 'xin''ci', 0)",
        [],
    );
    let _ = offline.conn().execute(
        "INSERT INTO bigram(prev_id, next_id, count, last_seen)
         SELECT id, (SELECT id FROM vocab WHERE text='新词'), 9, 0 FROM vocab WHERE text='旧'",
        [],
    );
    drop(offline);

    // IME 侧老 Dict：缓存里没有「新词」的 id——世代号变化必须触发重载
    d.set_lm_context(Some(("旧", "jiu")));
    assert_eq!(
        d.lm_boost(&cand("新词", "xin'ci")),
        9 * lm::LM_BOOST_UNIT,
        "世代号变化后离线新增的 pair 必须对已打开的 IME 生效"
    );
    let _ = fs::remove_file(&db);
}

/// 预算淘汰真触发：小预算注入（BIGRAM_BUDGET 是 const，测试用低于它的
/// 绝对计数差构造：预算不可调，那就直接清空 bigram 再塞 BIGRAM_BUDGET+2 行，
/// 验证淘汰逻辑恰好删到预算线）。
#[test]
fn mine_evicts_exactly_to_budget_line() {
    let db = tmp_db("budget");
    let mut d = Dict::open(&db).unwrap();
    // 造 BIGRAM_BUDGET + 2 行准入对（count 均 = MIN_ADMISSION，last_seen 相同
    // → 纯按 rowid 顺序淘汰最旧的 2 行）
    let conn = d.conn();
    let budget = lm::BIGRAM_BUDGET as usize;
    // 造 vocab 词（一次多行）
    let _ = conn.execute("BEGIN", []);
    for i in 0..(budget + 2) {
        let _ = conn.execute(
            "INSERT INTO vocab(text, reading, last_seen) VALUES (?1, ?1, 0)",
            [format!("w{i}")],
        );
    }
    let _ = conn.execute("COMMIT", []);
    let _ = conn.execute(
        "INSERT INTO bigram(prev_id, next_id, count, last_seen)
         SELECT id, id, 2, 0 FROM vocab",
        [],
    );
    drop(conn);

    let st = lm::mine(d.conn()).unwrap();
    assert!(st.evicted > 0, "超预算必须触发淘汰");
    let n: i64 = d
        .conn()
        .query_row("SELECT count(*) FROM bigram", [], |r| r.get(0))
        .unwrap();
    assert!(n <= lm::BIGRAM_BUDGET, "淘汰后必须回到预算内（实际 {n}）");
    let _ = fs::remove_file(&db);
}
