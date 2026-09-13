//! Word Lattice + Viterbi 句级联想
//!
//! 给定一段由 `segment` 产生的完整音节切分 `reading: &[String]`（长度 > 1），
//! 构建 Word Lattice 并在其上跑 Viterbi 最短路径，输出最优组合词序列。

use crate::dict::{Candidate, Dict};

/// 每条边的基础代价。每个词都收一份，构成「段数少优先」的固定偏置；
/// 概率项本身（每个多出来的词平均再加 ~10000）已经保证了不会乱切。
const BASE_COST: f64 = 10000.0;
/// 概率权重：-ln(p) 的放大系数，决定同样切分数下对高频词的偏好强度。
const FREQ_WEIGHT: f64 = 1000.0;
/// 每个词的额外惩罚，抑制把长串切成一堆单字。
const WORD_PENALTY: f64 = 50.0;

/// 路径的候选分数：把词频乘积折算成与单词 `freq` 同量纲、可同列排序的数值。
///
/// `score = √(∏ fᵢ) / T^((k-1)/4)`，再对 k≥3 的碎片路径每个额外词 ×1/100。
/// 两端都被实测否证过：旧的算术平均（`(Σfᵢ)/k`，千万级）让任何两个高频字的
/// 碎句压过一切真词；纯联合概率（`∏fᵢ/T^(k-1)`，几百度量级）又把所有组合
/// 埋到最生僻的补全词之下（`我们去` 486 < `我们确信` 2035 → 回到工单第 4 条的
/// 病态）。开方阻尼取几何平均与联合概率之间：实测 `我们去`≈6.5k 排进
/// `我们确信`(2k) 之前、`今天完`≈3k 让位给真词 `今天晚上`(51k)。
/// 二次修正（÷100 每多一个词）：词频高的单字把 k≥3 的分数抬进噪声区
/// （`最进好` 82k 反而压过真补全 `最近好吗`），而 3+ 词组合几乎全是碎切，
/// 打折把它们沉回补全之后。k 更大的长句（`我不知道你说的是什么`）分数
/// 落到地板也无妨——那类输入通常根本没有竞争候选。
fn sentence_score(ln_freq_sum: f64, words: usize, total: f64) -> u64 {
    let extra = words.saturating_sub(2) as f64;
    let score =
        (0.5 * ln_freq_sum - 0.25 * (words - 1) as f64 * total.ln() - 4.60517 * extra).exp();
    score.floor().max(1.0) as u64
}

/// 一条到某个音节节点的部分路径（k-best Viterbi 的 DP 状态）。
struct Path {
    cost: f64,
    text: String,
    pinyin: String,
    ln_freq_sum: f64,
    words: usize,
}

/// 每个音节节点保留的最优路径数。2 足以给「词库里没有整串词条」的输入
/// 多一条备选切分（`bucuobao` → 不错报 / 不错保），再多就是垃圾路了。
const PATHS_PER_NODE: usize = 2;

/// Viterbi 最短路径求解，返回最多 [`PATHS_PER_NODE`] 条文本互异的整句候选（代价升序）。
///
/// - `reading`: 由 `segment` 产生的完整音节切分，长度需 ≥ 2
/// - 代价函数：`cost = BASE_COST - ln(p) * FREQ_WEIGHT + WORD_PENALTY`，
///   其中 `p = freq / 语料总词频`。**必须是概率而不是计数**：多词路径的概率是相乘的，
///   用计数比就等于拿 `f(知)×f(道)` 压 `f(知道)`，切得越碎越占便宜（实测「知道」501,255
///   永远输给「知+道」4.4e6×1.06e6，整句吐出「只到」这类错字）。
/// - 词数惩罚：每个词 +50，鼓励合词而非全碎成单字
/// - 候选 `freq` 取 [`sentence_score`]，让整句在层一里按联合概率参与排序。
pub fn viterbi_sentences(dict: &Dict, reading: &[String]) -> Vec<Candidate> {
    if reading.len() < 2 {
        return Vec::new();
    }

    let n = reading.len();
    // dp[i] = 到节点 i 的最优若干条路径（代价升序、文本互异）
    let mut dp: Vec<Vec<Path>> = (0..n + 1).map(|_| Vec::new()).collect();
    dp[0].push(Path {
        cost: 0.0,
        text: String::new(),
        pinyin: String::new(),
        ln_freq_sum: 0.0,
        words: 0,
    });
    let mut reported_err = false;
    // 词频换算成概率才可比（见函数头对代价函数的说明）。一次查询，缓存住。
    let total = dict.total_freq() as f64;

    // 对每个起点 i，尝试所有终点 j (i < j <= n)。边恒向前：节点 i 处理完后
    // 不会再收到新路径，整段直接 take 走，避开 dp[i] 与 dp[j] 的双重可变借用。
    for i in 0..n {
        let prevs = std::mem::take(&mut dp[i]);
        if prevs.is_empty() {
            continue;
        }
        for j in (i + 1)..=n {
            let slice = &reading[i..j];
            // 查词库：最多取 5 个候选，按频次降序
            let cands = match dict.lookup(slice, 5) {
                Ok(c) => c,
                Err(e) => {
                    // 每次调用最多报一次：词库坏了会让每个跨度都失败，别刷屏
                    if !reported_err {
                        reported_err = true;
                        eprintln!("[kime] 警告：整句联想查词失败 ({e})，本次退化为逐词候选");
                    }
                    continue;
                }
            };
            // `Dict::lookup` 保证按 freq 降序返回，故 cands[0] 即该跨度最佳词。
            // 每个跨度只放一条最优边：备选路径的多样性由「节点保留 K 条前缀」提供。
            let Some(best) = cands.first() else { continue };
            // p = freq / 语料总词频。多词路径的概率是相乘的，所以「一个真词」
            // 与「两个高频单字」现在是同量纲比较，而不是计数比大小。
            let p = best.freq.max(1) as f64 / total;
            let edge_cost = BASE_COST - p.ln() * FREQ_WEIGHT + WORD_PENALTY;
            let best_ln = (best.freq.max(1) as f64).ln();
            for prev in prevs.iter() {
                let path = Path {
                    cost: prev.cost + edge_cost,
                    text: format!("{}{}", prev.text, best.text),
                    pinyin: if prev.pinyin.is_empty() {
                        best.pinyin.clone()
                    } else {
                        format!("{}'{}", prev.pinyin, best.pinyin)
                    },
                    ln_freq_sum: prev.ln_freq_sum + best_ln,
                    words: prev.words + 1,
                };
                insert_path(&mut dp[j], path);
            }
        }
    }

    let Some(finals) = dp.pop() else {
        return Vec::new();
    };
    finals
        .into_iter()
        .map(|p| Candidate {
            text: p.text,
            pinyin: p.pinyin,
            freq: sentence_score(p.ln_freq_sum, p.words, total),
            ai: false,
        })
        .collect()
}

/// 把一条完成路径按代价插入节点列表：同文本只留代价低的，列表长 ≤ [`PATHS_PER_NODE`]。
fn insert_path(list: &mut Vec<Path>, path: Path) {
    let pos = list.partition_point(|p| p.cost < path.cost);
    if list.len() >= PATHS_PER_NODE && pos >= PATHS_PER_NODE {
        return; // 已经容不下更好的
    }
    if list.iter().any(|p| p.text == path.text) {
        return; // 同一文本的更贵切分（不错+报 vs 不+错+报）不值得占名额
    }
    list.insert(pos, path);
    list.truncate(PATHS_PER_NODE);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::Dict;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_db(suffix: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "kime_lattice_test_{}_{}_{}.sqlite",
            suffix,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_file(&path);
        path
    }

    fn create_test_dict() -> (Dict, std::path::PathBuf) {
        let db = tmp_db("dict");
        let mut dict = Dict::open(&db).expect("open dict");
        // 使用 import 导入测试数据
        let yaml = r#"
...
你好	ni hao	1000
世界	shi jie	800
我	wo	5000
们	men	3000
爱	ai	2000
你好世界	ni hao shi jie	500
我爱你	wo ai ni	600
"#;
        let yaml_path = std::env::temp_dir().join(format!(
            "kime_lattice_yaml_{}_{}.yaml",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&yaml_path, yaml).unwrap();
        let _ = dict.import(&yaml_path);
        let _ = fs::remove_file(&yaml_path);
        (dict, db)
    }
    #[test]
    fn test_viterbi_two_words() {
        let (dict, db) = create_test_dict();
        // "ni'hao" + "shi'jie" -> "你好世界"
        let reading = vec![
            "ni".to_string(),
            "hao".to_string(),
            "shi".to_string(),
            "jie".to_string(),
        ];
        let cands = viterbi_sentences(&dict, &reading);
        assert_eq!(cands.first().map(|c| c.text.as_str()), Some("你好世界"));
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_viterbi_three_words() {
        let (dict, db) = create_test_dict();
        // "wo" + "ai" + "ni" -> "我爱你"
        let reading = vec!["wo".to_string(), "ai".to_string(), "ni".to_string()];
        let cands = viterbi_sentences(&dict, &reading);
        assert_eq!(cands.first().map(|c| c.text.as_str()), Some("我爱你"));
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_viterbi_single_syllable_returns_none() {
        let (dict, db) = create_test_dict();
        let reading = vec!["ni".to_string()];
        assert!(viterbi_sentences(&dict, &reading).is_empty());
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_viterbi_no_match_fallback() {
        let (dict, db) = create_test_dict();
        // 完全不在词库中的音节
        let reading = vec!["xxx".to_string(), "yyy".to_string()];
        assert!(viterbi_sentences(&dict, &reading).is_empty());
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_viterbi_partial_match() {
        let (dict, db) = create_test_dict();
        // "ni'hao" 在库里，"xxx" 不在
        let reading = vec!["ni".to_string(), "hao".to_string(), "xxx".to_string()];
        // 整句无法完整匹配，应返回空（当前实现要求全覆盖）
        assert!(viterbi_sentences(&dict, &reading).is_empty());
        let _ = fs::remove_file(&db);
    }
}
