//! jev 语义门控（#88）：准入候选对先过 jev 端点语义判定，再学库。
//!
//! 行为钉（§4）：
//! - `jev_gate_rejects_low_confidence`：mock 端点（wiremock）返回低置信 0.2
//!   → 对不入 bigram、`jev_gated` 计数；漏门控时红。
//! - `jev_gate_fallback_on_error`：不可达端点（http://127.0.0.1:1）→ 整批
//!   回退纯计数（`jev_skipped` 计数），挖掘不失败；模型挂时挖掘失败则红。
//! - `jev_phrases_stricter_threshold`：组词档 0.65 拒绝 / 0.75 接受
//!   （`JEV_PHRASE_THRESHOLD` = 0.7 严于 bigram 0.6）；两档混用时红。
//! - `no_flag_behavior_unchanged`：无 `--jev`（gate = None）时准入/组词/
//!   清日志全走纯计数，jev 计数为零——与合入前行为一致。
//! - 纯函数 seam `gate_pairs`（pairs + scores → accepted）手构置信表直测。
//!
//! 降级/准入路径的 HTTP 由 wiremock（dev-dep）mock；凭据（api key）从不
//! 入测试数据——`Jeving::new(…, None)`。

use kime_core::dict::Dict;
use kime_core::lm::{self, gate_pairs, Jeving};
use std::fs;
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockGuard, MockServer, ResponseTemplate};

fn tmp_db(suffix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "kime_jev_{}_{}_{}.sqlite3",
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

fn log_rows_left(d: &Dict) -> i64 {
    d.conn()
        .query_row("SELECT count(*) FROM commit_log", [], |r| r.get(0))
        .unwrap_or(0)
}

fn vocab_id(d: &Dict, text: &str) -> i64 {
    d.conn()
        .query_row("SELECT id FROM vocab WHERE text = ?1", [text], |r| r.get(0))
        .unwrap()
}

/// 「项目 → 进度」提交 3 次：ctx 路径准入候选 1 个（计数 3 ≥ MIN_ADMISSION）。
fn log_pair_3x(d: &mut Dict) {
    for _ in 0..3 {
        d.log_commit(None, &["xiang".into(), "mu".into()], "项目", None);
        d.log_commit(
            Some(("项目", "xiang'mu")),
            &["jin".into(), "du".into()],
            "进度",
            None,
        );
    }
}

/// 端点 mock：对 prompt（含「项目」）返回置信度 JSON 数组响应。
async fn mock_confidence(scores: &str, expect_calls: u64) -> (MockServer, MockGuard) {
    let server = MockServer::start().await;
    let mock = Mock::given(method("POST"))
        .and(body_string_contains("项目"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": scores}}]
        })))
        .expect(expect_calls)
        .mount_as_scoped(&server)
        .await;
    (server, mock)
}

/// 低置信对被门控拒绝：0.2 < 0.6 → 不入 bigram；被拒行的日志保留（下轮带
/// 更多证据再判），C2 无尾行照删。
#[tokio::test]
async fn jev_gate_rejects_low_confidence() {
    let db = tmp_db("jev_reject");
    let mut d = Dict::open(&db).unwrap();
    log_pair_3x(&mut d);

    let (server, guard) = mock_confidence("[0.2]", 1).await;
    let gate = Jeving::new(server.uri(), "jev-latest".into(), None);
    let st = lm::mine_gated(d.conn(), Some(&gate)).unwrap();

    assert_eq!(st.jev_gated, 1, "低置信对被门控拒绝");
    assert_eq!(st.jev_skipped, 0, "端点可达，无降级");
    assert_eq!(bigram_count(&d, "项目", "进度"), 0, "被拒对不入 bigram");
    assert_eq!(log_rows_left(&d), 3, "被拒对的日志行保留（C2 无尾行已删）");
    drop(guard); // wiremock 0.6.5：MockGuard drop 时同步验证 expect 计数
    server.stop().await;
    let _ = fs::remove_file(&db);
}

/// 降级路径：端点不可达 → 整批回退纯计数，挖掘照常完成（永不失败）。
#[test]
fn jev_gate_fallback_on_error() {
    let db = tmp_db("jev_fallback");
    let mut d = Dict::open(&db).unwrap();
    log_pair_3x(&mut d);

    // 端口 1 不可达：连接立即被拒（不依赖超时）
    let gate = Jeving::new(
        "http://127.0.0.1:1/v1/chat/completions".into(),
        "jev-latest".into(),
        None,
    );
    let st = lm::mine_gated(d.conn(), Some(&gate)).unwrap();

    assert_eq!(st.jev_skipped, 1, "降级批对数");
    assert_eq!(st.jev_gated, 0, "降级 = 纯计数，无拒绝");
    assert_eq!(bigram_count(&d, "项目", "进度"), 3, "降级对按纯计数准入");
    assert_eq!(
        log_rows_left(&d),
        0,
        "降级 = 已准入：对行照删（防双重计数不变式）"
    );
    let _ = fs::remove_file(&db);
}

/// 预置组词候选：bigram（项目,进度）count 10 —— 老化减半后 = 5，恰过
/// PHRASE_ADMISSION 线；日志无 ctx 对、无 tail 行 → bigram 门控批为空，
/// 只有组词门控一次 HTTP 调用。
fn seed_phrase_candidate(d: &mut Dict) {
    d.log_commit(None, &["xiang".into(), "mu".into()], "项目", None);
    d.log_commit(None, &["jin".into(), "du".into()], "进度", None);
    let p = vocab_id(d, "项目");
    let n = vocab_id(d, "进度");
    d.conn()
        .execute(
            "INSERT INTO bigram(prev_id, next_id, count, last_seen) VALUES (?1, ?2, 10, 0)",
            [p, n],
        )
        .unwrap();
}

async fn phrase_case(conf: &str, expect_learned: bool) {
    let suffix = if expect_learned {
        "jev_phrase_ok"
    } else {
        "jev_phrase_rej"
    };
    let db = tmp_db(suffix);
    let mut d = Dict::open(&db).unwrap();
    seed_phrase_candidate(&mut d);

    let (server, guard) = mock_confidence(conf, 1).await;
    let gate = Jeving::new(server.uri(), "jev-latest".into(), None);
    let st = lm::mine_gated(d.conn(), Some(&gate)).unwrap();

    let learned: i64 = d
        .conn()
        .query_row(
            "SELECT count(*) FROM phrase WHERE text = '项目进度' AND pinyin = 'xiang''mu''jin''du'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    if expect_learned {
        assert_eq!(st.phrases_learned, 1, "0.75 ≥ 0.7 组词阈值应学");
        assert_eq!(st.jev_gated, 0);
        assert_eq!(learned, 1, "词库出现「项目进度」");
    } else {
        assert_eq!(
            st.phrases_learned, 0,
            "0.65 < 0.7 组词阈值不应学（两档阈值不混用）"
        );
        assert_eq!(st.jev_gated, 1, "被组词档拒绝");
        assert_eq!(learned, 0, "词库无「项目进度」");
    }
    drop(guard); // MockGuard drop = 同步验证 expect 计数
    server.stop().await;
    let _ = fs::remove_file(&db);
}

/// 组词档比 bigram 档严：0.65 拒绝（≥0.6 bigram 线但 < 0.7 组词汇线）、
/// 0.75 接受。bug = 两档阈值混用（都按 0.6 / 都按 0.7）时红。
#[tokio::test]
async fn jev_phrases_stricter_threshold() {
    phrase_case("[0.65]", false).await;
    phrase_case("[0.75]", true).await;
}

/// 无 `--jev`（gate = None）：行为与 #88 合入前逐字节一致——纯计数准入/
/// 组词/清日志，jev 计数全零。
#[test]
fn no_flag_behavior_unchanged() {
    let db = tmp_db("jev_off");
    let mut d = Dict::open(&db).unwrap();
    for _ in 0..5 {
        d.log_commit(None, &["xiang".into(), "mu".into()], "项目", None);
        d.log_commit(
            Some(("项目", "xiang'mu")),
            &["jin".into(), "du".into()],
            "进度",
            None,
        );
    }
    let st = lm::mine(d.conn()).unwrap();

    assert_eq!(st.jev_gated, 0, "无门控：无拒计数");
    assert_eq!(st.jev_skipped, 0, "无门控：无降级计数");
    assert_eq!(bigram_count(&d, "项目", "进度"), 5, "纯计数准入");
    assert_eq!(st.phrases_learned, 1, "纯计数组词（≥ PHRASE_ADMISSION）");
    assert_eq!(log_rows_left(&d), 0, "准入行 + C2 行全清");
    let _ = fs::remove_file(&db);
}

/// 纯函数 seam：手构置信表钉两档阈值边界（0.59/0.60 bigram 界、
/// 0.65/0.75 组词界），无端点无 DB。
#[test]
fn gate_pairs_thresholds_pure() {
    let pairs: Vec<(String, String)> = vec![
        ("项目".into(), "进度".into()),
        ("项目".into(), "一次性".into()),
    ];
    let scores = [0.59_f64, 0.60];
    // bigram 档（0.6）：0.59 拒、0.60 收
    let accepted = gate_pairs(&pairs, &scores, kime_core::lm::JEV_BIGRAM_THRESHOLD);
    assert_eq!(accepted, vec![false, true]);
    // 组词档（0.7）：同一张置信表 0.59/0.60 全拒
    let accepted = gate_pairs(&pairs, &scores, kime_core::lm::JEV_PHRASE_THRESHOLD);
    assert_eq!(accepted, vec![false, false]);
    let scores = [0.65_f64, 0.75];
    // bigram 档全收，组词档 0.65 拒 0.75 收（两档分档钉）
    assert_eq!(
        gate_pairs(&pairs, &scores, kime_core::lm::JEV_BIGRAM_THRESHOLD),
        vec![true, true]
    );
    assert_eq!(
        gate_pairs(&pairs, &scores, kime_core::lm::JEV_PHRASE_THRESHOLD),
        vec![false, true]
    );
}
