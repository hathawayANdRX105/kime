//! Word Lattice + Viterbi 句级联想
//!
//! 给定一段由 `segment` 产生的完整音节切分 `reading: &[String]`（长度 > 1），
//! 构建 Word Lattice 并在其上跑 Viterbi 最短路径，输出最优组合词序列。

use crate::dict::{Candidate, Dict};

/// 每条边的基础代价。取值只需远大于 `FREQ_WEIGHT * ln(freq)` 的波动范围，
/// 保证「少切几个词」始终优先于「单词频率略高」。
const BASE_COST: f64 = 10000.0;
/// 词频权重：ln(freq) 的放大系数，决定同样切分数下对高频词的偏好强度。
const FREQ_WEIGHT: f64 = 1000.0;
/// 每个词的额外惩罚，抑制把长串切成一堆单字。
const WORD_PENALTY: f64 = 50.0;

/// Viterbi 最短路径求解，返回最优组合候选（作为第一候选上屏）。
///
/// - `reading`: 由 `segment` 产生的完整音节切分，长度需 ≥ 2
/// - 代价函数：`cost = 10000.0 - ln(max(freq, 1)) * 1000.0 + word_penalty`
/// - 词数惩罚：每个词 +50，鼓励合词而非全碎成单字
pub fn viterbi_sentence(dict: &Dict, reading: &[String]) -> Option<Candidate> {
    if reading.len() < 2 {
        return None;
    }

    let n = reading.len();
    // dp[i] = 从节点 0 到节点 i 的最小代价
    let mut dp = vec![f64::INFINITY; n + 1];
    // prev[i] = (前驱节点索引, 候选词文本, 候选词拼音, 候选词频率)
    let mut prev: Vec<Option<(usize, String, String, u64)>> = vec![None; n + 1];

    dp[0] = 0.0;
    let mut reported_err = false;

    // 对每个起点 i，尝试所有终点 j (i < j <= n)
    for i in 0..n {
        if dp[i].is_infinite() {
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
            if cands.is_empty() {
                continue;
            }
            // `Dict::lookup` 保证按 freq 降序返回，故 cands[0] 即该跨度最佳词
            let best = &cands[0];
            let freq = best.freq.max(1) as f64;
            let cost = BASE_COST - freq.ln() * FREQ_WEIGHT + WORD_PENALTY;
            let new_cost = dp[i] + cost;
            if new_cost < dp[j] {
                dp[j] = new_cost;
                prev[j] = Some((i, best.text.clone(), best.pinyin.clone(), best.freq));
            }
        }
    }

    // 如果终点不可达，返回 None
    if dp[n].is_infinite() || prev[n].is_none() {
        return None;
    }

    // 回溯重建路径
    let mut words = Vec::new();
    let mut pinyins = Vec::new();
    let mut total_freq = 0u64;
    let mut idx = n;
    while idx > 0 {
        if let Some((pi, ref text, ref pinyin, freq)) = prev[idx] {
            words.push(text.clone());
            pinyins.push(pinyin.clone());
            total_freq += freq;
            idx = pi;
        } else {
            break;
        }
    }
    words.reverse();
    pinyins.reverse();

    if words.is_empty() {
        return None;
    }

    // 组合成整句
    let text = words.join("");
    let pinyin = pinyins.join("'");

    // 频率取总频 / 词数作为整句频率的保守估计
    let sentence_freq = (total_freq as f64 / words.len() as f64).round() as u64;

    Some(Candidate {
        text,
        pinyin,
        freq: sentence_freq.max(1),
        ai: false,
    })
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
        let cand = viterbi_sentence(&dict, &reading);
        assert!(cand.is_some());
        let cand = cand.unwrap();
        assert_eq!(cand.text, "你好世界");
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_viterbi_three_words() {
        let (dict, db) = create_test_dict();
        // "wo" + "ai" + "ni" -> "我爱你"
        let reading = vec!["wo".to_string(), "ai".to_string(), "ni".to_string()];
        let cand = viterbi_sentence(&dict, &reading);
        assert!(cand.is_some());
        let cand = cand.unwrap();
        assert_eq!(cand.text, "我爱你");
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_viterbi_single_syllable_returns_none() {
        let (dict, db) = create_test_dict();
        let reading = vec!["ni".to_string()];
        let cand = viterbi_sentence(&dict, &reading);
        assert!(cand.is_none());
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_viterbi_no_match_fallback() {
        let (dict, db) = create_test_dict();
        // 完全不在词库中的音节
        let reading = vec!["xxx".to_string(), "yyy".to_string()];
        let cand = viterbi_sentence(&dict, &reading);
        assert!(cand.is_none());
        let _ = fs::remove_file(&db);
    }

    #[test]
    fn test_viterbi_partial_match() {
        let (dict, db) = create_test_dict();
        // "ni'hao" 在库里，"xxx" 不在
        let reading = vec!["ni".to_string(), "hao".to_string(), "xxx".to_string()];
        let cand = viterbi_sentence(&dict, &reading);
        // 整句无法完整匹配，应返回 None（当前实现要求全覆盖）
        assert!(cand.is_none());
        let _ = fs::remove_file(&db);
    }
}
